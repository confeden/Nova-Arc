<#
  Backs up the code-signing certificate, and exports its public half.

  RUN THIS YOURSELF, and put the .pfx somewhere OFFLINE — a password manager's
  file attachment, an encrypted drive, a safe. Not in this repository, not in a
  cloud repository, private or otherwise: a private repo is one leaked token,
  one careless fork or one compromised laptop away from being a public one, and
  whoever ends up with this key can sign anything at all as you. A self-signed
  certificate cannot be revoked, so there is no undo.

  Worth knowing before you bother: losing a SELF-SIGNED key costs almost
  nothing. It carries no reputation, so recovering from a lost PC is just
  running `tools\sign.ps1` again and getting a new certificate. The backup
  matters much more later, if a CA-issued certificate ever replaces it — and
  those are required to live on hardware tokens anyway.

  The password is asked for interactively and never written anywhere. Nothing
  here prints it, logs it, or defaults it.

      tools\backup-cert.ps1 -Path D:\somewhere-offline\nova-signing.pfx
#>

[CmdletBinding()]
param(
    # Where to write the private-key backup. Choose somewhere off this machine.
    [Parameter(Mandatory = $true)]
    [string]$Path,
    [string]$Subject = "CN=Brent, O=Nova Prism",
    # Where to write the PUBLIC certificate, which is safe to publish and is
    # what someone else needs to check a signature.
    [string]$PublicPath
)

$ErrorActionPreference = "Stop"

$cert = Get-ChildItem Cert:\CurrentUser\My |
    Where-Object { $_.Subject -eq $Subject -and $_.HasPrivateKey } |
    Sort-Object NotAfter -Descending |
    Select-Object -First 1

if (-not $cert) {
    throw "No certificate with a private key for $Subject. Run tools\sign.ps1 first."
}

Write-Host "Certificate : $($cert.Subject)"
Write-Host "Thumbprint  : $($cert.Thumbprint)"
Write-Host "Valid until : $($cert.NotAfter.ToString('yyyy-MM-dd'))"
Write-Host ""

if ($Path -like "*$([IO.Path]::DirectorySeparatorChar)*") {
    $dir = Split-Path -Parent $Path
    if ($dir -and -not (Test-Path $dir)) {
        throw "No such folder: $dir"
    }
}

# A repository is exactly where this must not go, and the mistake is easy to
# make with tab completion, so it is refused rather than warned about.
$repo = Split-Path -Parent $PSScriptRoot
$full = [IO.Path]::GetFullPath($Path)
if ($full.StartsWith([IO.Path]::GetFullPath($repo), [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to write a private key inside the repository ($full). Pick a path outside it."
}

Write-Host "The password protects the private key. It is not stored anywhere -"
Write-Host "if you lose it, the backup is unusable. Put it in a password manager now."
$pw = Read-Host -AsSecureString -Prompt "Password for the backup"
$pw2 = Read-Host -AsSecureString -Prompt "Again"

$a = [Runtime.InteropServices.Marshal]::PtrToStringUni([Runtime.InteropServices.Marshal]::SecureStringToBSTR($pw))
$b = [Runtime.InteropServices.Marshal]::PtrToStringUni([Runtime.InteropServices.Marshal]::SecureStringToBSTR($pw2))
$same = $a -ceq $b
$a = $null; $b = $null
if (-not $same) { throw "The two passwords differ." }

Export-PfxCertificate -Cert $cert -FilePath $Path -Password $pw -ChainOption EndEntityCertOnly | Out-Null
Write-Host ""
Write-Host "Private-key backup written: $Path"
Write-Host "Move it somewhere offline and delete any copy that syncs to a cloud."

if ($PublicPath) {
    Export-Certificate -Cert $cert -FilePath $PublicPath -Type CERT | Out-Null
    Write-Host "Public certificate (no private key, safe to publish): $PublicPath"
}
