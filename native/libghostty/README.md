# libghostty-vt native input

`manifest.toml` is the single source of truth for Splitlane's native
libghostty-vt inputs. Cargo never fetches Ghostty, runs bindgen, invokes Zig,
or mutates an artifact. Reviewed archives live under `prebuilt/<rust-target>/`,
so a standard checkout only verifies and links repository content.

The pinned source is Ghostty `v1.4.0`
(`83fb95624b0439ee660c2679b43b38c8b63769cd`) built with Zig 0.17.0 in
`ReleaseFast`. `bindings.rs` is pregenerated from the pinned C header. Its
normalized UTF-8 checksum is verified both in the workspace and in every
prepared artifact. Regenerate bindings only from that exact header, then
update `bindings_sha256` and every reviewed target.

## Linux archives

Prepare both reviewed Linux targets from a clean pinned checkout:

```sh
SPLITLANE_GHOSTTY_SOURCE_DIR=/path/to/ghostty scripts/build-libghostty-linux.sh
```

Outputs are written below `target/libghostty/<rust-target>/`. Cargo uses one
only when `SPLITLANE_LIBGHOSTTY_DIR` explicitly selects it; otherwise it uses
the repository artifact. `--verify-reproducible` builds from two clean Zig
caches and compares the normalized archive, header, bindings, and build info.
Zig cache paths are removed from ELF debug data with `eu-strip`, then members
are repacked with deterministic `ar` mode.

## Windows x64 MSVC archive

The production Windows input is the SIMD-enabled static archive
`prebuilt/x86_64-pc-windows-msvc/lib/ghostty-vt-static.lib`. Rebuild it from a
clean pinned checkout in an x64 Visual Studio environment:

```powershell
.\scripts\build-libghostty-windows.ps1 `
  -SourceDir C:\path\to\ghostty `
  -Zig C:\path\to\zig-0.17.0\zig.exe `
  -VerifyReproducible
```

The reviewed recipe requires:

| Input | Pinned value |
|---|---|
| Rust target | `x86_64-pc-windows-msvc` |
| Zig target | `x86_64-windows-msvc` |
| Zig | `0.17.0` |
| Zig distribution | official x64 Windows ZIP, SHA-256 `b5663f69581dcf391293fbf16c06cb80d81d806545ce618b4d0bab7f0eb8c428` |
| Zig executable SHA-256 | `f54b14f02a4d7d8ac62a94a0663486eb166bba3745d85b466238c1b93e723d46` |
| Zig codegen image base | `0x0000000140000000` |
| Zig PE DLL characteristics | `0x8160` |
| MSVC toolset | `14.38.33130` |
| Windows SDK | `10.0.26100.0` |
| Visual Studio LLVM tools | `19.1.5` |
| Optimization | `ReleaseFast` |
| SIMD | `true` |
| Build seed and jobs | `--seed=0 -j1` |

The upstream preparation command is:

```text
ZIG_LIB_DIR=<official-zig-lib> zig.exe build --verbose --seed=0 -j1 -Demit-lib-vt=true -Dtarget=x86_64-windows-msvc -Doptimize=ReleaseFast -Dsimd=true --prefix <fixed-prefix>
```

The CI downloads the manifest-pinned official Zig ZIP and verifies the archive,
executable, and PE metadata. The script exports the exact clean Ghostty commit
and builds it unmodified. Up to Ghostty `ae52f97d` on Zig 0.15.2 the recipe
also applied a formatter patch, because that compiler's ReleaseFast code
generation gave different register allocations across clean Windows builds.
The patch does not apply to v1.4.0 and was dropped with the move to Zig
0.17.0; the two-build comparison below is what would catch that drift if it
came back.

The export is built at fixed source, cache, and prefix paths. LLVM strips
debug data from every member of Ghostty's emitted fat archive, zeros each COFF
timestamp, and repacks ordinally sorted members with deterministic `llvm-ar
rcD` mode. It deliberately does not replay the emitted `build-lib` command.
Header and symbol inventories use the same ordinal, case-sensitive ordering,
so hashes do not depend on the Windows locale. Two complete builds start from
empty caches at the same canonical paths and must match byte for byte before
publication. The fixed `C:\Users\Public\splitlane-libghostty-83fb9562` source
path is part of the hash contract; the build aborts if that path is unavailable
or already occupied.

The fat archive contains Ghostty's Zig objects, Zig `compiler_rt`, simdutf,
and Highway. Zig 0.17 also nests the `ntdll.lib` and `kernel32.lib` import
libraries in the emitted archive; the recipe drops those two, and consumers
link them themselves. Zig 0.17 no longer stamps the C++ members with MSVC CRT
directives, so the recipe refuses only a directive that selects a CRT model
other than `MT_StaticRelease`, and the static smoke executable must import no
dynamic C or C++ runtime.
There is no `ghostty-vt.dll`; C consumers of the static header must define
`GHOSTTY_STATIC`.

`windows-smoke.c` is the minimal MSVC reproducer. The build script compiles it
with `/MT /W4 /WX`, initializes a terminal, parses a deterministic VT fixture,
and releases the terminal through Ghostty. It also verifies x64 COFF headers,
the required C symbol inventory, CRT directives, and the executable's DLL
dependencies. A missing SIMD object, unresolved system symbol, wrong
architecture, hash drift, or dynamic Ghostty dependency aborts the build and
preserves the temporary evidence directory.

The prepared Windows directory includes the archive, installed headers,
normalized bindings, `headers.sha256`, `symbols.txt`, and `build-info.txt`.
Each top-level input is pinned by `manifest.toml`; the header index covers all
installed headers. Publication replaces the complete previous target tree by
an atomic same-volume rename, so stale DLLs or metadata cannot survive a
rebuild. `splitlane-libghostty-sys/build.rs` selects this directory only for
`x86_64-pc-windows-msvc`, requires its exact file inventory, rejects incomplete
or incoherent metadata with an actionable rebuild command, and emits only
static plus reviewed system link directives.

## ABI and licensing

`splitlane-libghostty-sys` is the only raw ABI surface. The safe wrapper owns
every native handle, copies borrowed callback data before returning, releases
Ghostty allocations through their matching Ghostty destructor, catches Rust
panics inside callbacks, and validates API version, discriminants, callback
signatures, sizes, alignments, and field offsets before constructing a
terminal.

The manifest records the archive-member license inventory and pins
`THIRD_PARTY_NOTICES.md`. Packaging must reject an artifact whose reviewed
notice is absent, truncated, or stale.
