[CmdletBinding()]
param(
    [ValidateSet('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc')]
    [string]$Target = 'x86_64-pc-windows-msvc'
)

$ErrorActionPreference = 'Stop'
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$distRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot 'dist'))
$versionMatch = Select-String -LiteralPath (Join-Path $repoRoot 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $versionMatch) { throw 'No se pudo leer la version de Cargo.toml' }
$version = $versionMatch.Matches[0].Groups[1].Value
if ($version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?$') { throw 'Version no valida para empaquetar' }
$arch = $Target.Split('-')[0]
$packageName = "TerminalCanvas-$version-windows-$arch"
$binaryRoot = Join-Path $repoRoot "target/$Target/release"
$stageRoot = Join-Path $distRoot ('.stage-' + [Guid]::NewGuid().ToString('N'))
$stageRoot = [IO.Path]::GetFullPath($stageRoot)
if (-not $stageRoot.StartsWith($distRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Directorio temporal fuera de dist'
}

Push-Location $repoRoot
try {
    & cargo build --release --locked --bins --target $Target
    if ($LASTEXITCODE -ne 0) { throw 'Fallo cargo build' }
    $packageRoot = Join-Path $stageRoot $packageName
    New-Item -ItemType Directory -Path $packageRoot -Force | Out-Null
    foreach ($binary in @('mi-terminal.exe', 'tc-memory.exe', 'tc-memory-mcp.exe')) {
        $source = Join-Path $binaryRoot $binary
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw "Falta helper: $binary" }
        Copy-Item -LiteralPath $source -Destination (Join-Path $packageRoot $binary)
    }
    Copy-Item -LiteralPath (Join-Path $repoRoot 'LICENSE') -Destination $packageRoot
    Copy-Item -LiteralPath (Join-Path $repoRoot 'docs/PORTABLE.md') -Destination (Join-Path $packageRoot 'PORTABLE.md')
    $archive = Join-Path $distRoot "$packageName.zip"
    Compress-Archive -LiteralPath $packageRoot -DestinationPath $archive -Force
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText("$archive.sha256", "$hash  $packageName.zip`n", [Text.UTF8Encoding]::new($false))
    Write-Output "Paquete: $archive"
} finally {
    Pop-Location
    # stageRoot is an absolute, validated child of this repository's dist.
    if (Test-Path -LiteralPath $stageRoot) { Remove-Item -LiteralPath $stageRoot -Recurse -Force }
}
