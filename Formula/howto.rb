class Howto < Formula
  desc "Turn plain English into shell commands locally"
  homepage "https://github.com/jiwidi/howto"
  license "Apache-2.0"

  # The HEAD spec bootstraps the tap before the first stable release:
  #   brew install --HEAD jiwidi/tap/howto
  # Release automation adds an immutable source URL and checksum so the normal
  # one-command install becomes:
  #   brew install jiwidi/tap/howto
  head "https://github.com/jiwidi/howto.git", branch: "main"

  depends_on "cmake" => :build
  depends_on "rust" => :build
  depends_on "llama.cpp"

  def install
    system "cargo", "install", *std_cargo_args
    doc.install "README.md", "CHANGELOG.md", "CONTRIBUTING.md", "PRIVACY.md", "SECURITY.md"
    doc.install "LICENSE", "NOTICE", "THIRD_PARTY_LICENSES.txt", "docs"
    man1.install "man/howto.1"
    bash_completion.install "completions/howto.bash" => "howto"
    fish_completion.install "completions/howto.fish"
    zsh_completion.install "completions/_howto"
  end

  test do
    assert_match(
      /^howto \d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/,
      shell_output("#{bin}/howto --version").strip,
    )
    assert_match "USAGE:", shell_output("#{bin}/howto --help")
    assert_path_exists formula_opt_bin("llama.cpp")/"llama-server"
    %w[
      README.md CHANGELOG.md CONTRIBUTING.md PRIVACY.md SECURITY.md
      LICENSE NOTICE THIRD_PARTY_LICENSES.txt
    ].each do |document|
      assert_path_exists doc/document
    end
    assert_path_exists doc/"docs/ATTRIBUTION.md"
    assert_path_exists doc/"docs/TROUBLESHOOTING.md"
    assert_path_exists man1/"howto.1"
    assert_path_exists bash_completion/"howto"
    assert_path_exists fish_completion/"howto.fish"
    assert_path_exists zsh_completion/"_howto"

    ENV["HOWTO_HOME"] = testpath
    ENV.delete("HOWTO_MODEL")
    ENV.delete("HOWTO_PACKAGED_MODEL")
    model_status = shell_output("#{bin}/howto model status --json", 1)
    assert_match '"installed": false', model_status
  end
end
