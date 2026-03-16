# https://just.systems

set unstable
set dotenv-load

default:
    echo 'Hello, world!'

clear:
    clear

up: clear
    docker compose -f docker/compose.yml up -d --build --remove-orphans

down: clear
    docker compose -f docker/compose.yml down
