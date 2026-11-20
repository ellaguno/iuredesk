# Cambia cómo se autentica EWS y Autodiscover para probar cada camino de IureDesk:
#   basic     Basic + NTLM + Negotiate (lo de siempre)
#   ntlm      sin Basic: IureDesk pasa solo a NTLM
#   kerberos  sin Basic y solo Negotiate (como un dominio que restringe NTLM)
param([ValidateSet('basic', 'ntlm', 'kerberos')][string]$Modo = 'basic')
. Z:\comun.ps1
Add-PSSnapin Microsoft.Exchange.Management.PowerShell.SnapIn
$server = $env:COMPUTERNAME
$basic = $Modo -eq 'basic'
Set-WebServicesVirtualDirectory "$server\EWS (Default Web Site)" -BasicAuthentication $basic -WindowsAuthentication $true
Set-AutodiscoverVirtualDirectory "$server\Autodiscover (Default Web Site)" -BasicAuthentication $basic -WindowsAuthentication $true
$appcmd = "$env:windir\system32\inetsrv\appcmd.exe"
foreach ($site in @('Default Web Site/EWS', 'Default Web Site/Autodiscover')) {
    & $appcmd set config $site -section:system.webServer/security/authentication/windowsAuthentication /~providers /commit:apphost | Out-Null
    & $appcmd set config $site -section:system.webServer/security/authentication/windowsAuthentication /+"providers.[value='Negotiate']" /commit:apphost | Out-Null
    if ($Modo -ne 'kerberos') {
        & $appcmd set config $site -section:system.webServer/security/authentication/windowsAuthentication /+"providers.[value='NTLM']" /commit:apphost | Out-Null
    }
}
# Basta con reciclar los grupos de EWS y Autodiscover: un iisreset completo tarda tanto en esta VM
# que Windows lo da por fallido y deja IIS detenido.
Import-Module WebAdministration
foreach ($pool in 'MSExchangeServicesAppPool', 'MSExchangeAutodiscoverAppPool') { Restart-WebAppPool $pool }
Write-Host "EWS y Autodiscover en modo $Modo."
