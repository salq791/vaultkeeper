#!/usr/bin/env bash
# Full product loop against real tools: backup, snapshots, verify, restore.
set -euo pipefail
cd "$(dirname "$0")/.."
COMPOSE="docker compose -f docker-compose.smoke.yml"
VK="$COMPOSE exec -T vaultkeeper vaultkeeper"

cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then
    echo "== smoke failed (exit $status): recent container logs =="
    $COMPOSE logs --tail 100 || true
  fi
  $COMPOSE down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

$COMPOSE up -d --wait

echo "== validate bundled Supabase CLI and install deterministic smoke shim =="
$COMPOSE exec -T vaultkeeper supabase --version
$COMPOSE exec -T vaultkeeper sh -c '
  set -eu
  mkdir -p /tmp/smoke-bin
  cp /smoke/supabase-shim /tmp/smoke-bin/supabase
  chmod 0700 /tmp/smoke-bin/supabase
'

echo "== seed mongo =="
$COMPOSE exec -T source-mongo mongosh --quiet --eval \
  'try { rs.status() } catch (e) { rs.initiate({_id:"rs0",members:[{_id:0,host:"source-mongo:27017"}]}) }'
for _ in $(seq 1 30); do
  if $COMPOSE exec -T source-mongo mongosh --quiet --eval \
    'quit(db.hello().isWritablePrimary ? 0 : 1)' >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
$COMPOSE exec -T source-mongo mongosh --quiet --eval \
  'if (!db.hello().isWritablePrimary) { quit(1) }'
$COMPOSE exec -T source-mongo mongosh --quiet app --eval 'db.items.insertMany([{n:"a"},{n:"b"}])'

echo "== seed minio bucket via rclone in the vaultkeeper container =="
$COMPOSE exec -T vaultkeeper sh -c '
  export RCLONE_CONFIG_SEED_TYPE=s3 RCLONE_CONFIG_SEED_PROVIDER=Minio \
         RCLONE_CONFIG_SEED_ACCESS_KEY_ID=smokeadmin RCLONE_CONFIG_SEED_SECRET_ACCESS_KEY=smokesecret \
         RCLONE_CONFIG_SEED_ENDPOINT=http://minio:9000
  echo "object-one" > /tmp/obj1 && echo "object-two" > /tmp/obj2
  rclone mkdir SEED:smokebucket
  rclone copy /tmp/obj1 SEED:smokebucket/
  rclone copy /tmp/obj2 SEED:smokebucket/sub/
'

echo "== add sources =="
$VK init-repository
echo '{"password":"sourcepw"}' | $VK source add --name pg-src --engine postgres \
  --schedule "0 2 * * *" \
  --settings-json '{"host":"source-postgres","port":5432,"dbname":"app","user":"postgres"}' \
  --secrets-json -
echo '{"uri":"mongodb://source-mongo:27017/?replicaSet=rs0"}' | $VK source add --name mongo-src --engine mongodb \
  --schedule "0 2 * * *" --settings-json '{"oplog":true}' --secrets-json -
echo '{"access_key":"smokeadmin","secret_key":"smokesecret"}' | $VK source add --name store-src \
  --engine supabase_storage --schedule "0 2 * * *" \
  --settings-json '{"endpoint":"http://minio:9000","region":"us-east-1"}' --secrets-json -
echo '{"access_token":"smoke-token"}' | $VK source add --name fn-src \
  --engine supabase_functions --schedule "0 2 * * *" \
  --settings-json '{"project_ref":"smoke-project","local_functions_dir":"/project/functions","api_base":"http://mock-supabase-api"}' --secrets-json -

echo "== check-config must pass inside the container =="
$VK check-config

echo "== backups =="
$VK run --source pg-src
$VK run --source mongo-src
$VK run --source store-src
$COMPOSE exec -T vaultkeeper sh -c 'PATH=/tmp/smoke-bin:$PATH vaultkeeper run --source fn-src'

echo "== snapshots =="
SNAPS=$($VK snapshots)
echo "$SNAPS"
printf '%s\n' "$SNAPS" > /tmp/snaps
for tag in pg-src mongo-src store-src fn-src; do
  grep -q "source=$tag" /tmp/snaps || { echo "missing snapshot for $tag"; exit 1; }
done

echo "== verifies (scratch password is percent-encoded, sslmode honored) =="
$VK verify --source pg-src > /tmp/v1 2>&1 || { echo "pg verify failed"; cat /tmp/v1; exit 1; }
grep -q "tables=1" /tmp/v1 || { echo "pg verify metrics wrong"; cat /tmp/v1; exit 1; }
$VK verify --source mongo-src > /tmp/v2 2>&1 || { echo "mongo verify failed"; cat /tmp/v2; exit 1; }
grep -q "docs=2" /tmp/v2 || { echo "mongo verify metrics wrong"; cat /tmp/v2; exit 1; }
$VK verify --source store-src > /tmp/v3 2>&1 || { echo "storage verify failed"; cat /tmp/v3; exit 1; }
grep -q "files=2" /tmp/v3 || { echo "storage verify metrics wrong"; cat /tmp/v3; exit 1; }
$VK verify --source fn-src > /tmp/v4 2>&1 || { echo "functions verify failed"; cat /tmp/v4; exit 1; }
grep -q "supplemental_configs=1" /tmp/v4 || { echo "functions verify metrics wrong"; cat /tmp/v4; exit 1; }

echo "== restore leg: pg snapshot into a second scratch database =="
$COMPOSE exec -T scratch-postgres createdb -U verifier restored
VAULTKEEPER_RESTORE_TARGET='postgres://verifier:p%40ss%2Fword@scratch-postgres:5432/restored?sslmode=disable' \
  $COMPOSE exec -T -e VAULTKEEPER_RESTORE_TARGET vaultkeeper vaultkeeper restore --source pg-src --confirm-source pg-src
COUNT=$($COMPOSE exec -T scratch-postgres psql -U verifier -d restored -Atc "SELECT count(*) FROM items")
[ "$COUNT" = "3" ] || { echo "restored row count $COUNT != 3"; exit 1; }

echo "== restore leg: mongo snapshot into the scratch server =="
VAULTKEEPER_RESTORE_TARGET='mongodb://scratch-mongo:27017' \
  $COMPOSE exec -T -e VAULTKEEPER_RESTORE_TARGET vaultkeeper vaultkeeper restore --source mongo-src --confirm-source mongo-src
MONGO_COUNT=$($COMPOSE exec -T scratch-mongo mongosh --quiet app --eval 'db.items.countDocuments({})')
[ "$MONGO_COUNT" = "2" ] || { echo "restored mongo document count $MONGO_COUNT != 2"; exit 1; }

echo "== restore leg: storage snapshot repairs a deleted object =="
$COMPOSE exec -T vaultkeeper sh -c '
  export RCLONE_CONFIG_SEED_TYPE=s3 RCLONE_CONFIG_SEED_PROVIDER=Minio \
         RCLONE_CONFIG_SEED_ACCESS_KEY_ID=smokeadmin RCLONE_CONFIG_SEED_SECRET_ACCESS_KEY=smokesecret \
         RCLONE_CONFIG_SEED_ENDPOINT=http://minio:9000
  rclone deletefile SEED:smokebucket/obj1
'
$VK restore --source store-src --confirm-remote-overwrite
$COMPOSE exec -T vaultkeeper sh -c '
  export RCLONE_CONFIG_SEED_TYPE=s3 RCLONE_CONFIG_SEED_PROVIDER=Minio \
         RCLONE_CONFIG_SEED_ACCESS_KEY_ID=smokeadmin RCLONE_CONFIG_SEED_SECRET_ACCESS_KEY=smokesecret \
         RCLONE_CONFIG_SEED_ENDPOINT=http://minio:9000
  rclone cat SEED:smokebucket/obj1 | grep -q object-one
'

echo "== restore leg: functions export survives workdir cleanup =="
$VK restore --source fn-src
$COMPOSE exec -T vaultkeeper sh -c \
  'set -eu
   find /data/restores/fn-src -type f -path "*/supabase/functions/hello/index.ts" | grep -q .
   find /data/restores/fn-src -type f -path "*/supabase/functions/hello/deno.json" | grep -q .
   find /data/restores/fn-src -type f -name auth-config.json -exec grep -q smoke.invalid {} \;'

echo "SMOKE PASSED"
