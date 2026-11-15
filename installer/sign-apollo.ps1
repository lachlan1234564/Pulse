# Generates / reuses a local Apollo code-signing certificate and signs an EXE.
param(
  [Parameter(Mandatory = $true)][string]$ExePath,
  [Parameter(Mandatory = $true)][string]$CertDir,
  [Parameter(Mandatory = $true)][string]$PublicCerOut
)

$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force -Path $CertDir | Out-Null

$pfxPath = Join-Path $CertDir "apollo-codesign.pfx"
$passwordPlain = "ApolloTweaksLocalSign"
$secure = ConvertTo-SecureString -String $passwordPlain -Force -AsPlainText

if (-not (Test-Path $pfxPath)) {
  Write-Host "Creating Apollo Region code-signing certificate..."
  $cert = New-SelfSignedCertificate `
    -Type CodeSigningCert `
    -Subject "CN=Apollo Region, O=Apollo Region, C=US" `
    -KeyAlgorithm RSA `
    -KeyLength 2048 `
    -HashAlgorithm SHA256 `
    -CertStoreLocation "Cert:\CurrentUser\My" `
    -KeyExportPolicy Exportable `
    -NotAfter (Get-Date).AddYears(5)

  Export-PfxCertificate -Cert $cert -FilePath $pfxPath -Password $secure | Out-Null
  Export-Certificate -Cert $cert -FilePath $PublicCerOut -Type CERT | Out-Null
} else {
  Write-Host "Reusing existing PFX"
  $imported = Import-PfxCertificate -FilePath $pfxPath -CertStoreLocation "Cert:\CurrentUser\My" -Password $secure -Exportable
  Export-Certificate -Cert $imported -FilePath $PublicCerOut -Type CERT | Out-Null
}

$pfxData = Get-PfxData -FilePath $pfxPath -Password $secure
$signCert = $pfxData.EndEntityCertificates | Select-Object -First 1
if (-not $signCert) {
  $signCert = Import-PfxCertificate -FilePath $pfxPath -CertStoreLocation "Cert:\CurrentUser\My" -Password $secure -Exportable
}

Write-Host "Signing EXE..."
$result = Set-AuthenticodeSignature -FilePath $ExePath -Certificate $signCert -TimestampServer "http://timestamp.digicert.com" -HashAlgorithm SHA256
if ($result.Status -ne "Valid") {
  Write-Host "Timestamp may have failed. Retrying without timestamp..."
  $result = Set-AuthenticodeSignature -FilePath $ExePath -Certificate $signCert -HashAlgorithm SHA256
}

Write-Host ("Signature status: " + $result.Status)
Write-Host ("Public certificate: " + $PublicCerOut)

if (-not (Test-Path $PublicCerOut)) {
  throw "Public CER was not written"
}
