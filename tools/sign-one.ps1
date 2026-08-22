<#
  Signs ONE file. This is what `tauri build` calls through
  `bundle.windows.signCommand`, so the app binary and the installer are signed
  as they are produced — which is the only order that works, since the installer
  embeds the binary and has to see it already signed.

  It never CREATES a certificate: that belongs to `tools/sign.ps1`, run once by
  hand, because making a signing key is a decision and not a build step.

  A missing certificate is a WARNING, not a failure. Anyone who clones this
  repository has to be able to build it; what they get is an unsigned build,
  which is what they would have had anyway. The release checklist verifies the
  signature separately rather than trusting that this ran.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Path,
    [string]$Subject = "CN=Brent, O=Nova Prism"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $Path)) {
    Write-Warning "sign-one: nothing at $Path"
    exit 0
}

$cert = Get-ChildItem Cert:\CurrentUser\My |
    Where-Object { $_.Subject -eq $Subject -and $_.HasPrivateKey -and $_.NotAfter -gt (Get-Date) } |
    Sort-Object NotAfter -Descending |
    Select-Object -First 1

if (-not $cert) {
    Write-Warning "sign-one: no certificate for $Subject - leaving $([IO.Path]::GetFileName($Path)) unsigned. Run tools\sign.ps1 once to create one."
    exit 0
}

$signtool = Get-Command signtool.exe -ErrorAction SilentlyContinue
if ($signtool) {
    $signtool = $signtool.Source
}
else {
    # Newest SDK first: older signtool builds cannot do SHA-256 timestamping.
    $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^\d+\.' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName "x64\signtool.exe" } |
        Where-Object { Test-Path $_ } |
        Select-Object -First 1
}
if (-not $signtool) {
    Write-Warning "sign-one: signtool.exe not found - leaving $([IO.Path]::GetFileName($Path)) unsigned."
    exit 0
}


function Test-OurSignature {
    param([string]$Path, [string]$Thumbprint)
    # `signtool verify /pa` ALWAYS fails on a self-signed build: the chain
    # terminates in a root nobody trusts, and that is the whole point of being
    # self-signed. Treating it as failure would make every signed build error
    # out. What can be checked is what actually matters — that OUR certificate
    # signed it and the hash still matches the bytes:
    #   HashMismatch -> the file was modified after signing
    #   NotSigned    -> there is no signature
    #   UnknownError -> signed, hash fine, root untrusted (the normal case here)
    $sig = Get-AuthenticodeSignature -FilePath $Path
    if ($sig.Status -eq "NotSigned" -or $sig.Status -eq "HashMismatch") { return $false }
    return $sig.SignerCertificate -and $sig.SignerCertificate.Thumbprint -eq $Thumbprint
}

# Already signed by us? Tauri can hand the same file over twice, and a second
# signature would replace the first for no gain.
if (Test-OurSignature -Path $Path -Thumbprint $cert.Thumbprint) {
    Write-Host "sign-one: already signed, leaving it: $([IO.Path]::GetFileName($Path))"
    exit 0
}

# /tr + /td put an RFC 3161 timestamp on it, without which the signature dies
# with the certificate.
& $signtool sign /sha1 $cert.Thumbprint /fd sha256 `
    /tr http://timestamp.digicert.com /td sha256 `
    /d "Nova Prism" /du "https://t.me/nova_txt" $Path
if ($LASTEXITCODE -ne 0) {
    throw "signtool failed on $Path"
}
if (-not (Test-OurSignature -Path $Path -Thumbprint $cert.Thumbprint)) {
    throw "the signature on $Path did not take"
}
Write-Host "signed: $([IO.Path]::GetFileName($Path))"
