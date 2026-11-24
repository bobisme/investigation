investigation release candidate for Linux x86_64 (glibc)

investigation is an offline workbench for questions that need multiple
observations, competing explanations or options, and a handoff.

Contents
  bin/investigation  the executable
  install.sh         installs it into one prefix you name
  LICENSE            the license of investigation (MIT)
  NOTICES.md         third-party crates linked into the executable, with
                     their licenses
  PACKAGE.json       source commit, toolchain, platform and the glibc
                     version the executable needs
  SHA256SUMS         digests of every file above

Install
  sha256sum --check SHA256SUMS
  ./install.sh --prefix "$HOME/.local/investigation"
  export PATH="$HOME/.local/investigation/bin:$PATH"
  investigation onboard --format json

install.sh changes nothing outside the prefix. `inv` is only an optional
alias: ./install.sh --prefix DIR --alias-inv adds DIR/bin/inv only when no
other inv exists in DIR/bin or on PATH, and never replaces one.

Platforms
  Only Linux x86_64 with glibc is packaged and tested. PACKAGE.json lists the
  platforms that are not packaged and why. Use a local Linux filesystem for
  workspaces; see docs/support-matrix.md in the source for tested
  filesystems.

This is a release candidate. It is not published.
