{
  lib,
  rustPlatform,
  pkg-config,
  fontconfig,
  freetype,
  libxkbcommon,
  libxcb,
  wayland,
  libGL,
  vulkan-loader,
}:

let
  manifest = lib.importTOML ../Cargo.toml;
  # GPUI 运行时才 dlopen 这些库，链接器不会把它们写进 RUNPATH
  runtimeLibs = [
    wayland
    libGL
    vulkan-loader
  ];
in
rustPlatform.buildRustPackage {
  pname = "scell";
  inherit (manifest.workspace.package) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../xtask
      ../patches
      ../assets/icons
      ../packaging/linux
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;
  cargoBuildFlags = [
    "--package"
    "scell"
  ];

  nativeBuildInputs = [ pkg-config ];
  buildInputs = [
    fontconfig
    freetype
    libxkbcommon
    libxcb
    wayland
  ];

  # 测试在 CI 里跑；其中一部分要启动本机 shell，不适合放进构建沙箱
  doCheck = false;

  postInstall = ''
    install -Dm644 packaging/linux/scell.desktop -t $out/share/applications
    install -Dm644 assets/icons/scell.svg -t $out/share/icons/hicolor/scalable/apps
  '';

  # 放在 fixup 之后：fixup 会裁掉 RUNPATH 里看似没用到的目录
  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/scell
  '';

  passthru = { inherit runtimeLibs; };

  meta = {
    description = "More than fast: a clean, intuitive, all-in-one modern terminal";
    homepage = "https://github.com/Tailcoil/scell";
    license = lib.licenses.asl20;
    mainProgram = "scell";
    platforms = lib.platforms.linux;
  };
}
