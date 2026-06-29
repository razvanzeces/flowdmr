# FlowDMR build helpers. See README.md for the full deploy story.
#
# Assumes the `flowstation` repo is a sibling directory (../flowstation).
# Override with: make FLOWSTATION=/path/to/flowstation <target>

FLOWSTATION ?= ../flowstation

.PHONY: help sidecar test entity-test integrate revert clippy clean

help:
	@echo "FlowDMR make targets:"
	@echo "  make sidecar      build the sidecar binary (release) — no FlowStation needed"
	@echo "  make test         run sidecar + ipc tests"
	@echo "  make entity-test  test the FlowStation entity standalone (stub codec)"
	@echo "  make integrate    patch FlowStation to register the entity (FLOWSTATION=...)"
	@echo "  make revert       undo the FlowStation patch"
	@echo "  make clippy       lint everything"
	@echo ""
	@echo "Then build FlowStation with the injector:"
	@echo "  cd $(FLOWSTATION) && cargo build --release --features flowdmr   (device, real codec)"

# The sidecar + wire protocol — buildable on their own.
sidecar:
	cargo build --release -p flowdmr-sidecar

test:
	cargo test -p flowdmr-ipc -p flowdmr-sidecar

# The entity links the native tetra-codec by default; use the stub for a codec-less check.
entity-test:
	cd crates/flowdmr-entity && cargo test --no-default-features --features codec-stub

integrate:
	./integration/apply.sh "$(FLOWSTATION)"

revert:
	./integration/revert.sh "$(FLOWSTATION)"

clippy:
	cargo clippy -p flowdmr-ipc -p flowdmr-sidecar -- -D warnings
	cd crates/flowdmr-entity && cargo clippy --no-default-features --features codec-stub -- -D warnings

clean:
	cargo clean
	cd crates/flowdmr-entity && cargo clean
