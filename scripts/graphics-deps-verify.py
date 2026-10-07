#!/usr/bin/env python3
"""Catch features that reintroduce retired font/GPU dependencies into packages."""

from pathlib import Path
import tomllib


def verify(root: Path) -> None:
    with (root / "Cargo.toml").open("rb") as source:
        manifest = tomllib.load(source)
    with (root / "Cargo.lock").open("rb") as source:
        lock = tomllib.load(source)
    packages = {package["name"] for package in lock["package"]}
    retired = packages & {"paste", "ttf-parser"}
    if retired:
        raise ValueError(f"Retired graphics dependencies returned: {sorted(retired)}")

    # gpu-allocator accepts several incompatible windows crate versions. Cargo
    # can retain the old binding after a GPU upgrade, breaking Direct3D types.
    gpu = next(package for package in lock["package"] if package["name"] == "wgpu-hal")
    allocator = next(package for package in lock["package"] if package["name"] == "gpu-allocator")
    windows_binding = lambda package: {
        dependency for dependency in package["dependencies"]
        if dependency == "windows" or dependency.startswith("windows ")
    }
    if not windows_binding(gpu) or windows_binding(gpu) != windows_binding(allocator):
        raise ValueError("wgpu-hal and gpu-allocator must share their Direct3D Windows binding")

    eframe = manifest["dependencies"]["eframe"]
    required = {"accesskit", "default_fonts", "links", "wayland", "wgpu", "x11"}
    if eframe.get("default-features", True) or not required <= set(eframe["features"]):
        raise ValueError("Select graphics features explicitly and preserve native capabilities")
    linux = manifest["target"]['cfg(target_os = "linux")']["dependencies"]["winit"]
    features = set(linux["features"])
    if linux.get("default-features", True) or not {
        "wayland-dlopen", "wayland-csd-adwaita-crossfont"
    } <= features:
        raise ValueError("Keep Wayland title decorations using crossfont")
    if "wayland-csd-adwaita" in features or "wayland-csd-adwaita-notitle" in features:
        raise ValueError("Do not restore the old parser or disable decoration titles")


if __name__ == "__main__":
    verify(Path(__file__).resolve().parent.parent)
    print("Graphics dependency and native feature contracts passed")
