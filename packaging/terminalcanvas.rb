# Homebrew cask para TerminalCanvas (Ship-it 7.1, T4).
#
# Va en un tap propio: `homebrew-tap/Casks/terminalcanvas.rb`.
# Plantilla: no instalar hasta reemplazar ambos SHA256 con los checksums de
# una release firmada. scripts/bundle.sh --dmg genera un .sha256 por arquitectura.
cask "terminalcanvas" do
  arch arm: "aarch64", intel: "x86_64"
  version "1.2.0"
  sha256 arm: "REEMPLAZAR_CON_EL_SHA256_DEL_DMG_ARM64",
         intel: "REEMPLAZAR_CON_EL_SHA256_DEL_DMG_INTEL"

  url "https://github.com/MauroProto/terminal-canvas/releases/download/v#{version}/TerminalCanvas-#{version}-macos-#{arch}.dmg"
  name "TerminalCanvas"
  desc "Canvas de terminales para trabajar con varios agentes de codigo a la vez"
  homepage "https://github.com/MauroProto/terminal-canvas"

  # El checker interno informa y abre la descarga; la instalación es manual.
  depends_on macos: ">= :big_sur"

  app "TerminalCanvas.app"

  zap trash: [
    "~/Library/Application Support/terminal-app",
    "~/Library/Saved Application State/com.terminalcanvas.app.savedState",
  ]
end
