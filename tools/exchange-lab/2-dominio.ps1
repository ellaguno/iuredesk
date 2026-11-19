# Paso 2 (VM, PowerShell como administrador): IP fija en la red del anfitrión y bosque
# iuredesk.test. Reinicia sola al terminar.
. Z:\comun.ps1
$nat = Get-NetIPAddress -AddressFamily IPv4 | Where-Object IPAddress -like '10.0.2.*' | Select-Object -First 1
$hostonly = Get-NetAdapter | Where-Object { $_.Status -eq 'Up' -and $_.ifIndex -ne $nat.InterfaceIndex } | Select-Object -First 1
if (-not (Get-NetIPAddress -InterfaceIndex $hostonly.ifIndex -IPAddress 192.168.56.10 -ErrorAction SilentlyContinue)) {
    New-NetIPAddress -InterfaceIndex $hostonly.ifIndex -IPAddress 192.168.56.10 -PrefixLength 24 | Out-Null
}
Set-DnsClientServerAddress -InterfaceIndex $hostonly.ifIndex -ServerAddresses 127.0.0.1
Set-DnsClientServerAddress -InterfaceIndex $nat.InterfaceIndex -ServerAddresses 127.0.0.1
# La dirección de la red NAT no se publica en el DNS del dominio (el anfitrión llega por 192.168.56.10).
Set-DnsClient -InterfaceIndex $nat.InterfaceIndex -RegisterThisConnectionsAddress $false
Install-WindowsFeature AD-Domain-Services, DNS -IncludeManagementTools | Out-Null
Install-ADDSForest -DomainName iuredesk.test -DomainNetbiosName IUREDESK -InstallDns `
    -SafeModeAdministratorPassword (Secure $cfg.ADMIN_PASSWORD) -Force
