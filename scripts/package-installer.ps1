[CmdletBinding()]
param(
    [string]$PortableArchive,
    [string]$Compiler,
    [switch]$VerifyInstall
)

$ErrorActionPreference = 'Stop'
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$distRoot = Join-Path $repoRoot 'dist'
. (Join-Path $PSScriptRoot 'package-sign-windows.ps1')
$version = & python (Join-Path $PSScriptRoot 'package-verify.py') --version-only
if ($LASTEXITCODE -ne 0) { throw 'No se pudo leer la version de Cargo' }
$packageName = "TerminalCanvas-$version-windows-x86_64"
if (-not $PortableArchive) { $PortableArchive = Join-Path $distRoot "$packageName.zip" }
$PortableArchive = [IO.Path]::GetFullPath($PortableArchive)
if (-not $Compiler) {
    $command = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($command) { $Compiler = $command.Source }
    else { $Compiler = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6/ISCC.exe' }
}
if (-not (Test-Path -LiteralPath $Compiler -PathType Leaf)) { throw 'Se requiere Inno Setup 6 (ISCC.exe)' }
$stageRoot = [IO.Path]::GetFullPath((Join-Path $distRoot ('.installer-' + [Guid]::NewGuid().ToString('N'))))
if (-not $stageRoot.StartsWith($distRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Directorio temporal fuera de dist'
}
$installedRoot = $null
$uninstaller = $null
try {
    & python (Join-Path $PSScriptRoot 'package-verify.py') --archive $PortableArchive --target x86_64-pc-windows-msvc --extract-to $stageRoot
    if ($LASTEXITCODE -ne 0) { throw 'El ZIP portable no es valido' }
    $packageRoot = Join-Path $stageRoot $packageName
    $compilerArgs = @("/DAppVersion=$version", "/DAppNumericVersion=$($version.Split('-')[0])", "/DPackageRoot=$packageRoot", "/DOutputRoot=$distRoot")
    if ($env:TC_WINDOWS_CERT_THUMBPRINT) {
        Assert-TerminalCanvasWindowsSigningConfiguration
        foreach ($binary in @('mi-terminal.exe', 'tc-memory.exe', 'tc-memory-mcp.exe')) {
            Assert-TerminalCanvasWindowsSignature -Path (Join-Path $packageRoot $binary)
        }
        $signTool = Get-TerminalCanvasSignTool
        # $f is an Inno Setup substitution, passed literally to its signing hook.
        $signCommand = '"' + $signTool + '" sign /fd SHA256 /sha1 ' + $env:TC_WINDOWS_CERT_THUMBPRINT +
            ' /s My /tr "' + $env:TC_WINDOWS_TIMESTAMP_URL + '" /td SHA256 $f'
        $compilerArgs += @('/DSignedBuild', "/Sterminalcanvas=$signCommand")
    }
    $compilerArgs += (Join-Path $repoRoot 'packaging/windows/terminalcanvas.iss')
    & $Compiler @compilerArgs
    if ($LASTEXITCODE -ne 0) { throw 'Fallo Inno Setup' }
    $installer = Join-Path $distRoot "$packageName-setup.exe"
    if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) { throw 'Inno Setup no genero el instalador esperado' }
    if ($env:TC_WINDOWS_CERT_THUMBPRINT) { Assert-TerminalCanvasWindowsSignature -Path $installer }
    $hash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText("$installer.sha256", "$hash  $packageName-setup.exe`n", [Text.UTF8Encoding]::new($false))
    if ($VerifyInstall) {
        # This installs/uninstalls a real product registration. Limit the smoke
        # test to an ephemeral GitHub runner, never a developer's installed app.
        if ($env:GITHUB_ACTIONS -ne 'true' -or -not $env:RUNNER_TEMP) { throw '-VerifyInstall requiere un runner GitHub efimero' }
        $runnerTemp = [IO.Path]::GetFullPath($env:RUNNER_TEMP)
        $installedRoot = [IO.Path]::GetFullPath((Join-Path $runnerTemp ('tc-install-' + [Guid]::NewGuid().ToString('N'))))
        if (-not $installedRoot.StartsWith($runnerTemp + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Directorio de instalacion fuera de RUNNER_TEMP'
        }
        $logPath = Join-Path $runnerTemp 'terminalcanvas-installer.log'
        $process = Start-Process -FilePath $installer -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', "/DIR=`"$installedRoot`"", "/LOG=`"$logPath`"") -PassThru -WindowStyle Hidden
        if (-not $process.WaitForExit(120000)) { $process.Kill(); throw 'Timeout de instalacion' }
        if ($process.ExitCode -ne 0) { throw "Fallo instalacion: $($process.ExitCode); log: $logPath" }
        $uninstaller = Join-Path $installedRoot 'unins000.exe'
        foreach ($source in Get-ChildItem -LiteralPath $packageRoot -File) {
            $installed = Join-Path $installedRoot $source.Name
            if ((Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $source.FullName -Algorithm SHA256).Hash) {
                throw "Contenido instalado distinto del portable: $($source.Name)"
            }
        }
        if ($env:TC_WINDOWS_CERT_THUMBPRINT) { Assert-TerminalCanvasWindowsSignature -Path $uninstaller }
        & python (Join-Path $PSScriptRoot 'package-verify.py') --smoke-directory $installedRoot
        if ($LASTEXITCODE -ne 0) { throw 'Fallo la verificacion de helpers instalados' }
        Write-Output 'Instalacion y helpers verificados'
    }
    Write-Output "Instalador: $installer"
} finally {
    if ($installedRoot -and (Test-Path -LiteralPath (Join-Path $installedRoot 'unins000.exe') -PathType Leaf)) {
        $uninstaller = Join-Path $installedRoot 'unins000.exe'
        $process = Start-Process -FilePath $uninstaller -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART') -PassThru -WindowStyle Hidden
        if (-not $process.WaitForExit(120000)) { $process.Kill(); throw 'Timeout de desinstalacion' }
        if ($process.ExitCode -ne 0) { throw "Fallo desinstalacion: $($process.ExitCode)" }
        foreach ($binary in @('mi-terminal.exe', 'tc-memory.exe', 'tc-memory-mcp.exe')) {
            if (Test-Path -LiteralPath (Join-Path $installedRoot $binary)) { throw "Desinstalacion incompleta: $binary" }
        }
        Write-Output 'Desinstalacion verificada'
    }
    if (Test-Path -LiteralPath $stageRoot) { Remove-Item -LiteralPath $stageRoot -Recurse -Force }
}
