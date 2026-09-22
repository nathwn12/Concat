# FORK.md — the Windows build and installer recipe

Reached from [`AGENTS.md`](AGENTS.md). What it holds is mechanical and rarely
read, which is why it is not in the always-loaded contract. The workflow at
`.github/workflows/build-app.yml` is the source of truth for the exact commands;
this is the readable condensation of its Windows steps, for a machine that is not
a runner.

## Prerequisites

| What | Environment variable | Notes |
|---|---|---|
| Rust | — | stable; `rust-version` in `src/Cargo.toml` |
| FFmpeg development libraries | `FFMPEG_DIR` | BtbN's `shared` build for `win64`: headers, import libraries and the runtime DLLs in one archive. `ffmpeg-sys` honours `FFMPEG_DIR` ahead of pkg-config. Put `%FFMPEG_DIR%\bin` on `PATH` so the binary finds the DLLs when it runs. |
| LLVM, for libclang | `LIBCLANG_PATH` | `C:\Program Files\LLVM\bin` on a default install; bindgen needs it |
| cmake and a C++ toolchain | — | MSVC — whisper.cpp is compiled in by its crate at build time |
| sherpa-onnx, shared | `SHERPA_ONNX_LIB_DIR` | import libraries (`.lib`) only |
| sherpa-onnx, shared | `SHERPA_DLL_DIR` | runtime DLLs (`.dll`) only |

The split between the two sherpa directories is load-bearing, not tidiness. When
the sys crate's build script sees a DLL beside the `.lib` files, it copies them
into the "profile directory", which it locates by searching `OUT_DIR`'s ancestors
for one named after `$PROFILE` — and under this workspace's custom `app` profile
that directory is not called `release`, so the search panics. Keep the DLLs apart
and stage them beside the binary yourself.

The version to fetch is the one `src/Cargo.lock` resolved for `sherpa-onnx-sys`.
The archive is
`sherpa-onnx-v<version>-win-x64-shared-MT-Release-lib.tar.bz2` from the
project's releases; unpack it with something that speaks bzip2, because Windows'
own `tar` pipes through an external decoder and has been seen to hang on it —
Python's `tarfile` unpacks it in-process.

## Build

```sh
cd src
cargo build --profile app -p concat
```

Run from `src/`, the workspace root — not the repository root. `app` is the
shipping profile: it inherits `release` and adds fat LTO, `panic = "abort"` and
stripped symbols. Each costs build time and is there for the binary that ships;
the workspace `Cargo.toml` says what each one buys. For a build you intend to
profile, use `--profile release` instead. The binary lands at
`src\target\app\concat.exe`.

## Stage

Make a folder named for the bundle, `Concat-<version>-windows-x86_64`, and put in
it exactly what the installer will install:

| From | What |
|---|---|
| `src\target\app\concat.exe` | the binary |
| `%FFMPEG_DIR%\bin\*.dll` | FFmpeg's runtime libraries |
| `%SHERPA_DLL_DIR%\*.dll` | sherpa-onnx's runtime libraries |
| `LICENSE`, `THIRD_PARTY_NOTICES.md` | the licence texts |

## Installer — Inno Setup

`assets\windows\concat.iss`, compiled by Inno Setup 6:

```powershell
& "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" `
  "/DVersion=<version>" "/DArch=x64compatible" "/DSuffix=x86_64" `
  "/DStage=$PWD\stage\Concat-<version>-windows-x86_64" "/DOut=$PWD\stage" `
  assets\windows\concat.iss
```

All five defines are required and the file errors without them. `Arch` is Inno's
word for the machine — `x64compatible` for the x86_64 build, which also installs
on ARM PCs under emulation, and `arm64` for the native one — while `Suffix` is the
bundle's word, and names the file: `Concat-<version>-windows-<suffix>-setup.exe`
in `Out`.

The `AppId` is one id for the life of the product, so an install over an older one
is an upgrade and not a second copy — which is what lets a locally-labelled build
replace a shipped one in place (see `AGENTS.md` on labels).
`PrivilegesRequired=lowest` makes it a per-user install, into
`%LOCALAPPDATA%\Programs\Concat`, asking for no administrator; the desktop icon is
an opt-in task on the tasks page. The installer reads `assets\icons\concat.ico`
and `LICENSE` from the repository rather than from the staged folder.

## Installer — WiX (.msi)

The per-machine package an administrator deploys, built from the same staged
folder:

```powershell
wix build -arch x64 -pdbtype none -d "Version=<version>" -d "Root=$PWD" `
  -d "Stage=$PWD\stage\Concat-<version>-windows-x86_64" `
  -o "stage\Concat-<version>-windows-x86_64.msi" assets\windows\concat.wxs
```

`Scope="perMachine"` puts it in `Program Files`, with a Start menu entry and a
clean uninstall; one `UpgradeCode` for the life of the product means a newer
`.msi` replaces an older one. `Root` is the repository, because WiX resolves a
source path against the directory it is run from, not against the `.wxs`.
