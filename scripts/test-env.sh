#!/bin/sh
# scripts/test-env.sh up|down: Docker servers for trying SSH tunnels (see test-env.yml). The SSH key it makes
# lives in .test-env/, which is not committed.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
export KEYS="$here/../.test-env"
compose() { docker compose -f "$here/test-env.yml" "$@"; }

case "${1:-}" in
  up)
    mkdir -p "$KEYS"
    [ -f "$KEYS/id_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -f "$KEYS/id_ed25519"
    compose up -d --wait
    echo "SSH: 127.0.0.1:2222, user tunnel, password tunnelpass or key $KEYS/id_ed25519"
    echo "Postgres: host postgres, port 5432, user postgres, password testpass, database app"
    echo "MySQL: host mysql, port 3306, user root, password testpass, database app"
    echo "ClickHouse: http://127.0.0.1:8123, user tm, password testpass, database app"
    echo "SQL Server: 127.0.0.1:1433 (or host mssql through SSH), user sa, password TestPass1!word"
    echo "CockroachDB: 127.0.0.1:26257 (or host cockroach through SSH), user root, no password, sslmode=disable"
    ;;
  down) compose down -v ;;
  *) echo "usage: $0 up|down" >&2; exit 2 ;;
esac
