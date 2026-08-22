<#
  Signs Nova Prism's binaries and installer with a SELF-SIGNED certificate.

  WHAT THIS BUYS, AND WHAT IT DOES NOT — worth reading before deciding to run it.

  It buys:
    * a publisher name in the UAC prompt instead of "Unknown publisher";
    * tamper evidence — a signed file that was modified stops verifying, so
      "is this the build Brent published?" becomes a question Windows answers;
    * a timestamp, so the signature stays valid after the certificate expires.

  It does NOT buy:
    * silence from SmartScreen. "Windows protected your PC" comes from
      REPUTATION, which is built from a certificate issued by a public CA plus
      download volume. A self-signed certificate has no reputation and cannot
      acquire any. Anyone installing the beta will still click "More info" ->
      "Run anyway", and the release notes should say so plainly rather than let
      people discover it.

  The private key stays in the current user's certificate store and is never
  exported here: no .pfx, no password, nothing to leak into the repository.
  Backing it up is a separate decision — without the key, a later build simply
  gets a new certificate and a different thumbprint.

  ORDER MATTERS. The installer embeds the binaries, so they have to be signed
  BEFORE it is built:

      cargo build --release
      npm --prefix ui run build
      tools\sign.ps1 -Stage binaries
      cd crates\nova-gui; ..\..\ui\node_modules\.bin\tauri build; cd ..\..
      tools\sign.ps1 -Stage installer

  Running it with no arguments does both stages and warns if the installer is
  older than the binaries — which is exactly the mistake that produces a signed
  installer full of unsigned executables.
#>

[CmdletBinding()]
param(
    # Who the certificate says signed it. Shown in the UAC prompt.
    [string]$Subject = "CN=Brent, O=Nova Prism",
    # "binaries" | "installer" | "all"
    [ValidateSet("binaries", "installer", "all")]
    [string]$Stage = "all",
    # Also write the PUBLIC certificate next to the installer, so anyone can
    # check a download against it. It carries no private key.
    [switch]$ExportPublic
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$release = Join-Path $root "target\release"
$bundle = Join-Path $release "bundle\nsis"

function Find-SignTool {
    $cmd = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    # Newest SDK first: older signtool builds cannot do SHA-256 timestamping.
    $candidates = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^\d+\.' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName "x64\signtool.exe" } |
        Where-Object { Test-Path $_ }
    if ($candidates) { return $candidates[0] }
    throw "signtool.exe not found. Install the Windows SDK component 'Windows SDK Signing Tools for Desktop Apps'."
}

function Get-SigningCert {
    param([string]$Subject)
    $existing = Get-ChildItem Cert:\CurrentUser\My |
        Where-Object { $_.Subject -eq $Subject -and $_.HasPrivateKey -and $_.NotAfter -gt (Get-Date) } |
        Sort-Object NotAfter -Descending |
        Select-Object -First 1
    if ($existing) {
        Write-Host "Using the certificate already in your store: $($existing.Thumbprint)"
        return $existing
    }
    Write-Host "No usable certificate for $Subject - creating one."
    Write-Host "It goes into YOUR certificate store (Cert:\CurrentUser\My). No admin rights, nothing system-wide."
    # Five years: long enough not to be a chore, short enough that a leaked key
    # is not forever. Timestamped signatures outlive it either way.
    $cert = New-SelfSignedCertificate `
        -Type CodeSigningCert `
        -Subject $Subject `
        -CertStoreLocation Cert:\CurrentUser\My `
        -KeyExportPolicy Exportable `
        -KeyUsage DigitalSignature `
        -KeyAlgorithm RSA `
        -KeyLength 3072 `
        -HashAlgorithm SHA256 `
        -NotAfter (Get-Date).AddYears(5)
    Write-Host "Created: $($cert.Thumbprint)"
    return $cert
}

function Invoke-Sign {
    param([string]$SignTool, [string]$Thumbprint, [string[]]$Paths)
    foreach ($p in $Paths) {
        if (-not (Test-Path $p)) {
            Write-Warning "skipped (not built): $p"
            continue
        }
        Write-Host "signing $p"
        # /tr + /td: an RFC 3161 timestamp, without which every signature dies
        # with the certificate. /fd sha256 because SHA-1 is refused by Windows.
        & $SignTool sign /sha1 $Thumbprint /fd sha256 `
            /tr http://timestamp.digicert.com /td sha256 `
            /d "Nova Prism" /du "https://t.me/nova_txt" $p
        if ($LASTEXITCODE -ne 0) { throw "signtool failed on $p" }
        # NOT `signtool verify /pa`: on a self-signed build that always fails,
        # because the chain ends in a root nobody trusts — which is what being
        # self-signed means. What is checkable is that OUR certificate signed
        # it and the hash still matches the bytes.
        $sig = Get-AuthenticodeSignature -FilePath $p
        if ($sig.Status -eq "NotSigned" -or $sig.Status -eq "HashMismatch" -or
            -not $sig.SignerCertificate -or $sig.SignerCertificate.Thumbprint -ne $Thumbprint) {
            throw "the signature on $p did not take ($($sig.Status))"
        }
    }
}

$signtool = Find-SignTool
Write-Host "signtool: $signtool"
$cert = Get-SigningCert -Subject $Subject

$binaries = @(
    (Join-Path $release "nova.exe"),
    (Join-Path $release "nova-prism.exe")
)
$installers = @(Get-ChildItem $bundle -Filter "*setup.exe" -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName })

if ($Stage -in @("binaries", "all")) {
    Invoke-Sign -SignTool $signtool -Thumbprint $cert.Thumbprint -Paths $binaries
}

if ($Stage -in @("installer", "all")) {
    if (-not $installers) {
        Write-Warning "no installer in $bundle - build it with 'tauri build' first"
    }
    else {
        # The installer carries copies of the binaries. If it was built before
        # they were signed, signing it now produces a signed wrapper around
        # unsigned executables — which is the failure people only notice on
        # someone else's machine.
        foreach ($inst in $installers) {
            $instTime = (Get-Item $inst).LastWriteTimeUtc
            foreach ($b in $binaries) {
                if ((Test-Path $b) -and ((Get-Item $b).LastWriteTimeUtc -gt $instTime)) {
                    Write-Warning "$([IO.Path]::GetFileName($inst)) is OLDER than $([IO.Path]::GetFileName($b)): rebuild the installer, or it holds unsigned binaries"
                }
            }
        }
        Invoke-Sign -SignTool $signtool -Thumbprint $cert.Thumbprint -Paths $installers
    }
}

if ($ExportPublic -and $installers) {
    $cer = Join-Path $bundle "nova-prism-public.cer"
    Export-Certificate -Cert $cert -FilePath $cer -Type CERT | Out-Null
    Write-Host "public certificate (no private key): $cer"
}

Write-Host ""
Write-Host "Done. Windows will still show SmartScreen's 'Windows protected your PC' on a"
Write-Host "self-signed build - that warning is about reputation, not about the signature."
Write-Host "What changed is that the publisher now has a name and the file can be checked"
Write-Host "for tampering."
