#!/bin/sh
# Lets the standby service in docker-compose.test.yml stream from this
# server over the compose network. Test-only: never use this pg_hba line
# on a real server.
echo "host replication all all scram-sha-256" >> "$PGDATA/pg_hba.conf"
