# Shared Authenticode support for portable executables and the Inno Setup installer.
# The release workflow imports a real certificate into CurrentUser/My first.
function Get-TerminalCanvasSignTool {
    $command = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    $sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/bin'
    $versions = @(Get-ChildItem -LiteralPath $sdkRoot -Directory | Where-Object { $_.Name -match '^10\.\d+\.\d+\.\d+$' } | Sort-Object { [version]$_.Name } -Descending)
    foreach ($version in $versions) {
        $candidate = Join-Path $version.FullName 'x64/signtool.exe'
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { return $candidate }
    }
    throw 'No se encontro signtool.exe del Windows SDK'
}

function Assert-TerminalCanvasWindowsSigningConfiguration {
    if ($env:TC_WINDOWS_CERT_THUMBPRINT -notmatch '^[0-9a-fA-F]{40}$') {
        throw 'Se requiere el thumbprint de un certificado Authenticode real'
    }
    $timestamp = $null
    if (-not [Uri]::TryCreate($env:TC_WINDOWS_TIMESTAMP_URL, [UriKind]::Absolute, [ref]$timestamp) -or
        $timestamp.Scheme -ne 'https' -or $env:TC_WINDOWS_TIMESTAMP_URL -match '[\s"$]') {
        throw 'Se requiere una URL HTTPS de timestamp RFC3161 sin espacios'
    }
    $certificate = Get-Item -LiteralPath "Cert:/CurrentUser/My/$env:TC_WINDOWS_CERT_THUMBPRINT"
    if (-not $certificate.HasPrivateKey -or $certificate.NotAfter -le [DateTime]::Now -or $certificate.NotBefore -gt [DateTime]::Now) {
        throw 'El certificado de firma no tiene clave privada vigente'
    }
    if ('1.3.6.1.5.5.7.3.3' -notin @($certificate.EnhancedKeyUsageList | ForEach-Object { [string]$_.ObjectId })) {
        throw 'El certificado no habilita firma de codigo'
    }
}

function Assert-TerminalCanvasWindowsSignature {
    param([Parameter(Mandatory)][string]$Path)
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Thumbprint -ne $env:TC_WINDOWS_CERT_THUMBPRINT -or
        -not $signature.TimeStamperCertificate) {
        throw "Firma Authenticode o timestamp invalidos: $Path"
    }
}

function Invoke-TerminalCanvasWindowsSigning {
    param([Parameter(Mandatory)][string]$Path)
    Assert-TerminalCanvasWindowsSigningConfiguration
    $signTool = Get-TerminalCanvasSignTool
    & $signTool sign /fd SHA256 /sha1 $env:TC_WINDOWS_CERT_THUMBPRINT /s My /tr $env:TC_WINDOWS_TIMESTAMP_URL /td SHA256 $Path
    if ($LASTEXITCODE -ne 0) { throw "Fallo la firma de $Path" }
    Assert-TerminalCanvasWindowsSignature -Path $Path
}
