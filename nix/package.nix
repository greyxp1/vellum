{
  installShellFiles,
  lib,
  libxkbcommon,
  makeBinaryWrapper,
  pkg-config,
  rustPlatform,
  versionCheckHook,
  vulkan-loader,
  wayland,
}:

let
  cargoToml = fromTOML (builtins.readFile ../Cargo.toml);
in
rustPlatform.buildRustPackage {
  pname = cargoToml.package.name;
  inherit (cargoToml.package) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../build.rs
      ../build_support.rs
      ../default-config.toml
      ../src
      ../xtask/Cargo.toml
      ../xtask/src
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  __structuredAttrs = true;

  nativeBuildInputs = [
    installShellFiles
    makeBinaryWrapper
    pkg-config
  ];

  buildInputs = [
    libxkbcommon
    wayland
  ];

  postInstall = ''
    outputDir=$(find target -type d -path '*/build/vellum-*/out' \
      -exec test -f '{}/completions/vellum.bash' ';' -print -quit)
    installManPage "$outputDir"/man/*.1
    installShellCompletion "$outputDir"/completions/vellum.{bash,fish,nu} \
      --zsh "$outputDir"/completions/_vellum
    install -Dm644 "$outputDir"/completions/vellum.elv \
      $out/share/elvish/lib/vellum.elv
    install -Dm644 "$outputDir"/completions/_vellum.ps1 \
      $out/share/powershell/vellum.Completion.ps1
    install -Dm644 default-config.toml \
      $out/share/doc/vellum/default-config.toml

    wrapProgram $out/bin/vellum \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath [vulkan-loader wayland]}
  '';

  doInstallCheck = true;
  nativeInstallCheckInputs = [versionCheckHook];

  meta = {
    inherit (cargoToml.package) description;
    homepage = cargoToml.package.repository;
    license =
      with lib.licenses;
      AND [
        isc
        mit
      ];
    mainProgram = "vellum";
    platforms = lib.platforms.linux;
  };
}
