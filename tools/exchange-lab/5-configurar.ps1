# Paso 5 (VM, IUREDESK\Administrator): certificado, URL, autenticación de EWS y Autodiscover,
# buzones de prueba y el certificado para el anfitrión (Z:\iuredesk-lab.crt).
. Z:\comun.ps1
Add-PSSnapin Microsoft.Exchange.Management.PowerShell.SnapIn
$server = $env:COMPUTERNAME
try { Add-DnsServerResourceRecordA -ZoneName iuredesk.test -Name autodiscover -IPv4Address 192.168.56.10 -ErrorAction Stop } catch { }

# Certificado propio con los nombres que usa IureDesk (el anfitrión lo agrega a sus raíces). Si el
# paso ya corrió, se reutiliza (se puede volver a correr).
$cert = Get-ExchangeCertificate | Where-Object FriendlyName -eq 'IureDesk lab' | Select-Object -First 1
if (-not $cert) {
    $cert = New-ExchangeCertificate -FriendlyName 'IureDesk lab' -SubjectName 'cn=ex1.iuredesk.test' `
        -DomainName ex1.iuredesk.test, autodiscover.iuredesk.test, iuredesk.test -PrivateKeyExportable $true -Force
}
Enable-ExchangeCertificate -Thumbprint $cert.Thumbprint -Services IIS -Force
$raw = (Get-Item "Cert:\LocalMachine\My\$($cert.Thumbprint)").RawData
$b64 = [Convert]::ToBase64String($raw) -replace '(.{64})', "`$1`n"
Set-Content -Path Z:\iuredesk-lab.crt -Value "-----BEGIN CERTIFICATE-----`n$($b64.TrimEnd())`n-----END CERTIFICATE-----" -Encoding ascii

# URL y autenticación: Basic + Windows (NTLM y Negotiate). 6-modo.ps1 cambia esto para probar.
$ews = 'https://ex1.iuredesk.test/EWS/Exchange.asmx'
Set-WebServicesVirtualDirectory "$server\EWS (Default Web Site)" -InternalUrl $ews -ExternalUrl $ews `
    -BasicAuthentication $true -WindowsAuthentication $true
Set-AutodiscoverVirtualDirectory "$server\Autodiscover (Default Web Site)" -BasicAuthentication $true -WindowsAuthentication $true
Set-ClientAccessService $server -AutoDiscoverServiceInternalUri 'https://autodiscover.iuredesk.test/Autodiscover/Autodiscover.xml'
# Kerberos por el nombre de Autodiscover (el de ex1 ya lo cubre HOST/ex1).
setspn -S HTTP/autodiscover.iuredesk.test $server | Out-Null

# Buzones de prueba.
foreach ($u in @(@{ A = 'ana'; N = 'Ana'; L = 'Pérez'; P = $cfg.ANA_PASSWORD }, @{ A = 'bea'; N = 'Bea'; L = 'Ruiz'; P = $cfg.BEA_PASSWORD })) {
    if (-not (Get-Mailbox $u.A -ErrorAction SilentlyContinue)) {
        New-Mailbox -Name "$($u.N) $($u.L)" -Alias $u.A -UserPrincipalName "$($u.A)@iuredesk.test" -FirstName $u.N `
            -LastName $u.L -Password (Secure $u.P) -ResetPasswordOnNextLogon $false | Out-Null
    }
}
iisreset /noforce
Write-Host 'Listo. En el anfitrión: paso 6 del README (hosts y certificado).'
