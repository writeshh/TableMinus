#!/bin/sh
# Runs the Rust tests marked #[ignore] that need MySQL, Postgres or Redis on their default ports. Starts from
# test-env.yml (the `live` profile) whichever of them isn't already listening; a server of your own (DBngin, Homebrew)
# needs the same setup: passwordless root/postgres, MySQL 8.0 with local_infile on, PostGIS. The tests for the other
# engines need `scripts/test-env.sh up`, the SSH tunnel test has its own script, test-ssh.sh, and the two Unix socket
# tests need a DBngin-style socket on the machine: all of those are skipped here.
set -eu
cd "$(dirname "$0")/.."
need=""
nc -z 127.0.0.1 3306 2>/dev/null || need="$need live-mysql"
nc -z 127.0.0.1 5432 2>/dev/null || need="$need live-postgres"
nc -z 127.0.0.1 6379 2>/dev/null || need="$need live-redis"
# --wait returns once each container's healthcheck passes. KEYS is only read by the sshd service.
# shellcheck disable=SC2086 # one word per service
[ -z "$need" ] || KEYS=/dev/null docker compose -f scripts/test-env.yml --profile live up -d --wait $need
cd src-tauri
cargo test --locked -- --ignored \
  --skip tunnel_to_local_mysql \
  --skip driver::clickhouse:: --skip driver::mssql:: --skip driver::libsql:: --skip driver::bigquery:: \
  --skip mongo::tests:: --skip elastic::tests:: --skip dynamo::tests:: --skip cassandra::tests:: \
  --skip local_libsql_ --skip local_cockroach_ --skip cockroach_process_list_ --skip local_probe_should_recognise_mongodb \
  --skip local_mysql_socket_should_be_found --skip local_postgres_should_connect_through_its_socket
