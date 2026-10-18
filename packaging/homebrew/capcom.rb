# Homebrew cask TEMPLATE (not published). To use it, put it in a tap repo
# (e.g. SiCuellar/homebrew-tap/Casks/capcom.rb), fill in version and sha256
# (from SHA256SUMS.txt on the GitHub Release), then: brew install --cask SiCuellar/tap/capcom
cask "capcom" do
  version "0.1.0"
  sha256 "REPLACE_WITH_SHA256_OF_Capcom-macos-universal.zip"

  url "https://github.com/SiCuellar/capcom/releases/download/v#{version}/Capcom-macos-universal.zip"
  name "Capcom"
  desc "Hold a key, talk, get clean text — with your own speech-to-text and LLM servers"
  homepage "https://github.com/SiCuellar/capcom"

  depends_on macos: ">= :ventura"

  app "Capcom.app"
  binary "#{appdir}/Capcom.app/Contents/MacOS/capcom"

  # Builds are ad-hoc signed and not notarized.
  postflight do
    system_command "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "#{appdir}/Capcom.app"]
  end

  uninstall quit: "dev.capcom.client"

  zap trash: [
    "~/.config/capcom",
    "~/Library/Application Support/capcom",
    "~/Library/Logs/Capcom.log",
  ]

  caveats <<~EOS
    Next: run `capcom setup`, then open Capcom from /Applications and grant
    Microphone, Accessibility and Input Monitoring when macOS asks.
  EOS
end
