SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c

REGISTRY ?= ghcr.io/trustless-engineering
GIT_SHA ?= $(shell git rev-parse HEAD)
PLATFORMS ?= linux/amd64,linux/arm64
LOCAL_PLATFORM ?= linux/amd64

APP_IMAGE := $(REGISTRY)/leader-stream:$(GIT_SHA)
K8S_DIR ?= k8s

.PHONY: all build build-app push push-app deploy print-vars wasm

all: deploy

build: build-app

build-app:
	docker buildx build --platform $(LOCAL_PLATFORM) -t $(APP_IMAGE) -f Dockerfile . --load

push: push-app

push-app:
	docker buildx build --platform $(PLATFORMS) -t $(APP_IMAGE) -f Dockerfile . --push

deploy: push
	kubectl kustomize $(K8S_DIR) | sed 's/\$${GIT_SHA}/$(GIT_SHA)/g' | kubectl apply -f -

print-vars:
	@echo "GIT_SHA=$(GIT_SHA)"
	@echo "APP_IMAGE=$(APP_IMAGE)"

wasm:
	cd leader-stream && \
		rustup target add wasm32-unknown-unknown && \
		cargo build --release --target wasm32-unknown-unknown --lib && \
		wasm-bindgen --target no-modules --out-name app --out-dir ./public ./target/wasm32-unknown-unknown/release/leader_stream.wasm && \
		if ! grep -q 'wasm_bindgen("/app_bg.wasm");' ./public/app.js; then \
			printf '\nwasm_bindgen("/app_bg.wasm");\n' >> ./public/app.js; \
		fi
