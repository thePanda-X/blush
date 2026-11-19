# blush — build & install. Just run `make`.
#
#   make              build (release) and install for this OS
#   make build        release build only
#   make install      install the built binary
#   make uninstall    remove the installed binary
#   make adapter      pre-install / update the Claude Code ACP adapter
#   make check        verify prerequisites (cargo, node, npm, claude)
#   make test         run unit tests
#   make web          rebuild the web companion (needs bun; the built files are committed)
#   make clean        cargo clean
#
# Override the install location with BINDIR=..., e.g.
#   make BINDIR=/usr/local/bin        (macOS/Linux)
#   make BINDIR=C:\tools\bin          (Windows)

APP := blush

ifeq ($(OS),Windows_NT)
    DETECTED_OS := Windows
else
    UNAME_S := $(shell uname -s)
    ifeq ($(UNAME_S),Darwin)
        DETECTED_OS := macOS
    else
        DETECTED_OS := $(UNAME_S)
    endif
endif

# ---------------------------------------------------------------- Windows
ifeq ($(DETECTED_OS),Windows)
SHELL := powershell.exe
.SHELLFLAGS := -NoProfile -ExecutionPolicy Bypass -Command
EXE := .exe
CARGO_HOME ?= $(USERPROFILE)\.cargo
BINDIR ?= $(CARGO_HOME)\bin
BIN := target\release\$(APP)$(EXE)
DEST := $(BINDIR)\$(APP)$(EXE)

.PHONY: all build install uninstall adapter check test clean web

all: install

check:
	@foreach ($$t in 'cargo','node','npm') { if (-not (Get-Command $$t -ErrorAction SilentlyContinue)) { Write-Error "$$t not found on PATH"; exit 1 } }
	@if (-not (Get-Command claude -ErrorAction SilentlyContinue)) { Write-Host 'note: `claude` CLI not found - fine, but use /login in blush to sign in' }
	@Write-Host "OS: $(DETECTED_OS)  |  node $$(node --version)  |  $$(cargo --version)"

build: check
	cargo build --release --locked

install: build
	@New-Item -ItemType Directory -Force -Path '$(BINDIR)' | Out-Null
	@if (Test-Path '$(DEST)') { try { Move-Item -Force '$(DEST)' '$(DEST).old' -ErrorAction Stop } catch {} }
	@Copy-Item -Force '$(BIN)' '$(DEST)'
	@if (Test-Path '$(DEST).old') { try { Remove-Item -Force '$(DEST).old' -ErrorAction Stop } catch {} }
	@Write-Host "Installed $(APP) -> $(DEST)"
	@$$p = [Environment]::GetEnvironmentVariable('Path','User'); if (($$env:Path -split ';') -notcontains '$(BINDIR)' -and ($$p -split ';') -notcontains '$(BINDIR)') { [Environment]::SetEnvironmentVariable('Path', "$$p;$(BINDIR)", 'User'); Write-Host 'Added $(BINDIR) to your user PATH - open a new terminal to use it.' }
	@Write-Host 'Run: $(APP)'

uninstall:
	@if (Test-Path '$(DEST)') { Remove-Item -Force '$(DEST)' }
	@Write-Host "Removed $(DEST)"

adapter: build
	& '$(BIN)' --update-adapter

test:
	cargo test

web:
	cd web; bun install; bun run build

clean:
	cargo clean

# ---------------------------------------------------------------- macOS / Linux
else
SHELL := /bin/sh
CARGO_HOME ?= $(HOME)/.cargo
BINDIR ?= $(CARGO_HOME)/bin
BIN := target/release/$(APP)
DEST := $(BINDIR)/$(APP)

.PHONY: all build install uninstall adapter check test clean web

all: install

check:
	@for t in cargo node npm; do command -v $$t >/dev/null 2>&1 || { echo "error: $$t not found on PATH"; exit 1; }; done
	@command -v claude >/dev/null 2>&1 || echo "note: \`claude\` CLI not found - fine, but use /login in blush to sign in"
	@echo "OS: $(DETECTED_OS)  |  node $$(node --version)  |  $$(cargo --version)"

build: check
	cargo build --release --locked

install: build
	@mkdir -p "$(BINDIR)"
	@if [ -w "$(BINDIR)" ]; then install -m 755 "$(BIN)" "$(DEST)"; else sudo install -m 755 "$(BIN)" "$(DEST)"; fi
ifeq ($(DETECTED_OS),macOS)
	@# Clear quarantine and ad-hoc sign so Gatekeeper doesn't kill the copied binary.
	@xattr -d com.apple.quarantine "$(DEST)" 2>/dev/null || true
	@codesign --force --sign - "$(DEST)" >/dev/null 2>&1 || true
endif
	@echo "Installed $(APP) -> $(DEST)"
	@case ":$$PATH:" in *":$(BINDIR):"*) ;; *) echo "warning: $(BINDIR) is not on your PATH. Add this to your shell profile:"; echo "  export PATH=\"$(BINDIR):\$$PATH\"";; esac
	@echo "Run: $(APP)"

uninstall:
	@rm -f "$(DEST)" 2>/dev/null || sudo rm -f "$(DEST)"
	@echo "Removed $(DEST)"

adapter: build
	"./$(BIN)" --update-adapter

test:
	cargo test

web:
	cd web && bun install && bun run build

clean:
	cargo clean
endif
