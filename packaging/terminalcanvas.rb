# Homebrew cask para TerminalCanvas (Ship-it 7.1, T4).
#
# Va en un tap propio: `homebrew-tap/Casks/terminalcanvas.rb`.
# Después de cada release hay que actualizar `version` y `sha256` (el script
# scripts/bundle.sh --dmg imprime el sha256 del dmg y lo deja en un .sha256).
cask "terminalcanvas" do
  version "1.2.0"
  sha256 "REEMPLAZAR_CON_EL_SHA256_DEL_DMG"

  url "https://github.com/MauroProto/terminal-canvas/releases/download/v#{version}/TerminalCanvas-#{version}.dmg"
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
