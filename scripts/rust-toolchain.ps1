function Get-TerminalCanvasRustToolchain {
    param([Parameter(Mandatory = $true)][string]$RepoRoot)

    $pin = Select-String -LiteralPath (Join-Path $RepoRoot 'rust-toolchain.toml') -Pattern '^\s*channel\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $pin -or $pin.Matches[0].Groups[1].Value -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') {
        throw 'rust-toolchain.toml must pin an exact stable Rust version'
    }
    $channel = $pin.Matches[0].Groups[1].Value
    $cargo = (& rustup which --toolchain $channel cargo) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw "Cannot resolve Cargo $channel with rustup" }
    $rustc = (& rustup which --toolchain $channel rustc) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw "Cannot resolve Rust $channel with rustup" }
    foreach ($executable in @($cargo, $rustc)) {
        if (-not [IO.Path]::IsPathRooted($executable) -or -not (Test-Path -LiteralPath $executable -PathType Leaf)) {
            throw "Toolchain executable not found: $executable"
        }
    }
    $rustcVersion = (& $rustc -vV) -join "`n"
    if ($LASTEXITCODE -ne 0 -or $rustcVersion -notmatch '(?m)^release: (\S+)\r?$' -or $Matches[1] -ne $channel) {
        throw "Expected Rust $channel; found $rustcVersion"
    }
    $cargoVersion = (& $cargo --version) -join "`n"
    if ($LASTEXITCODE -ne 0 -or $cargoVersion -notmatch '^cargo\s+(\S+)' -or $Matches[1] -ne $channel) {
        throw "Expected Cargo $channel; found $cargoVersion"
    }
    [PSCustomObject]@{ Channel = $channel; Cargo = $cargo; Rustc = $rustc }
}
