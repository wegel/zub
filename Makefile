.PHONY: build

CONTAINER_ENGINE := $(shell command -v podman 2>/dev/null || command -v docker 2>/dev/null)
IMAGE_NAME := zub-builder
PROFILE ?= release

ifeq ($(PROFILE),release)
  CARGO_FLAGS := --release
else
  CARGO_FLAGS :=
endif

build:
	$(CONTAINER_ENGINE) build -t $(IMAGE_NAME) -f Containerfile .
	$(CONTAINER_ENGINE) run --rm \
		-v $(PWD):/src:Z \
		-v $(HOME)/.cargo/registry:/root/.cargo/registry:Z \
		$(IMAGE_NAME) \
		cargo build $(CARGO_FLAGS) --target x86_64-unknown-linux-musl
