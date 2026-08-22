{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  wayland,
  wayland-protocols,
  libxkbcommon,
  vulkan-loader,
}: let
  pname = "vellum";
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../build.rs
      ../src
    ];
  };

  cargoToml = fromTOML (builtins.readFile ../Cargo.toml);
in
  rustPlatform.buildRustPackage {
    inherit pname src;
    version = cargoToml.package.version;

    cargoLock.lockFile = ../Cargo.lock;

    nativeBuildInputs = [
      pkg-config
      makeWrapper
    ];

    buildInputs = [
      wayland
      wayland-protocols
      libxkbcommon
      vulkan-loader
    ];

    postInstall = ''
      wrapProgram $out/bin/vellum \
        --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath [wayland vulkan-loader]}

      man_dir=$(find target -type d -path '*/build/vellum-*/out/man' -print -quit)
      mkdir -p "$out/share/man/man1"
      install -m644 "$man_dir"/*.1 "$out/share/man/man1/"
    '';

    meta.platforms = lib.platforms.linux;
  }
