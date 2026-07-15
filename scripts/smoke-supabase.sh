#!/bin/sh
set -eu

[ "${SUPABASE_ACCESS_TOKEN:-}" = "smoke-token" ]
[ "$#" -eq 5 ]
[ "$1" = "functions" ]
[ "$2" = "download" ]
[ "$3" = "--use-api" ]
[ "$4" = "--project-ref" ]
[ "$5" = "smoke-project" ]

mkdir -p supabase/functions/hello
cp /project/functions/hello/index.ts supabase/functions/hello/index.ts
