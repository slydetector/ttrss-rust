#!/bin/sh -e
# Tear down the oracle stack. The db and webroot volumes are kept by default
# (the golden dataset survives). Use 'reset' to wipe volumes too.
cd "$(dirname "$0")/.."

if [ "$1" = "reset" ]; then
    docker compose down --volumes --remove-orphans
else
    docker compose down --remove-orphans
fi
