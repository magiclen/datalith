EXECUTABLE_NAME := datalith
PREFIX ?= /usr/local

.PHONY: all install test check fmt clean

all:
	cargo build --locked --release -p $(EXECUTABLE_NAME)

install: all
	install -Dm755 target/release/$(EXECUTABLE_NAME) $(DESTDIR)$(PREFIX)/bin/$(EXECUTABLE_NAME)

test:
	cargo test --locked --workspace

check:
	cargo +nightly fmt --all -- --check
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

fmt:
	cargo +nightly fmt --all

clean:
	cargo clean
