.ONESHELL:
# BINARY=""

.DEFAULT_GOAL: all

all:
	TAG_NAME=local cargo build --release
	chown -R $$(id -u):$$(id -g) target

test:
	TAG_NAME=local cargo test
	chown -R $$(id -u):$$(id -g) target
