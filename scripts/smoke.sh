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

echo "== seed mongo =="
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
echo '{"password":"sourcepw"}' | $VK source add --name pg-src --engine postgres \
  --schedule "0 2 * * *" \
  --settings-json '{"host":"source-postgres","port":5432,"dbname":"app","user":"postgres"}' \
  --secrets-json -
echo '{"uri":"mongodb://source-mongo:27017/app"}' | $VK source add --name mongo-src --engine mongodb \
  --schedule "0 2 * * *" --settings-json '{"db":"app"}' --secrets-json -
echo '{"access_key":"smokeadmin","secret_key":"smokesecret"}' | $VK source add --name store-src \
  --engine supabase_storage --schedule "0 2 * * *" \
  --settings-json '{"endpoint":"http://minio:9000","region":"us-east-1"}' --secrets-json -

echo "== check-config must pass inside the container =="
$VK check-config

echo "== backups =="
$VK run --source pg-src
$VK run --source mongo-src
$VK run --source store-src

echo "== snapshots =="
SNAPS=$($VK snapshots)
echo "$SNAPS"
printf '%s\n' "$SNAPS" > /tmp/snaps
for tag in pg-src mongo-src store-src; do
  grep -q "source=$tag" /tmp/snaps || { echo "missing snapshot for $tag"; exit 1; }
done

echo "== verifies (scratch password is percent-encoded, sslmode honored) =="
$VK verify --source pg-src > /tmp/v1 2>&1 || { echo "pg verify failed"; cat /tmp/v1; exit 1; }
grep -q "tables=1" /tmp/v1 || { echo "pg verify metrics wrong"; cat /tmp/v1; exit 1; }
$VK verify --source mongo-src > /tmp/v2 2>&1 || { echo "mongo verify failed"; cat /tmp/v2; exit 1; }
grep -q "docs=2" /tmp/v2 || { echo "mongo verify metrics wrong"; cat /tmp/v2; exit 1; }
$VK verify --source store-src > /tmp/v3 2>&1 || { echo "storage verify failed"; cat /tmp/v3; exit 1; }
grep -q "files=2" /tmp/v3 || { echo "storage verify metrics wrong"; cat /tmp/v3; exit 1; }

echo "== restore leg: pg snapshot into a second scratch database =="
$COMPOSE exec -T scratch-postgres createdb -U verifier restored
VAULTKEEPER_RESTORE_TARGET='postgres://verifier:p%40ss%2Fword@scratch-postgres:5432/restored?sslmode=disable' \
  $COMPOSE exec -T -e VAULTKEEPER_RESTORE_TARGET vaultkeeper vaultkeeper restore --source pg-src
COUNT=$($COMPOSE exec -T scratch-postgres psql -U verifier -d restored -Atc "SELECT count(*) FROM items")
[ "$COUNT" = "3" ] || { echo "restored row count $COUNT != 3"; exit 1; }

echo "SMOKE PASSED"
