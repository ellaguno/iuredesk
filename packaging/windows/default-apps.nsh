; Integración de IureDesk con Windows 10/11 (R1.3). cargo-packager inserta este texto en su
; installer.nsi como `preinstall_section` (ver `cargo xtask package windows`): va antes de la
; sección «Install» y las secciones «un.» van al desinstalador (antes de «Uninstall»). Todo es por
; usuario (SHCTX = HKCU con `installMode: currentUser`) y no muestra nada: `/S` sigue funcionando.
;
; - Protocolo `iuredesk:` (enlaces de las notificaciones y de otras apps de la suite).
; - `mailto:` y archivos .eml, .ics, .vcf como **opción** en «Aplicaciones predeterminadas»
;   (Capabilities + RegisteredApplications) y en «Abrir con» (OpenWithProgids). Windows no deja que
;   un instalador se imponga como predeterminado: lo elige el usuario.
; - AppUserModelID `${IDENTIFIER}` (com.iurefficient.IureDesk) con nombre e icono, para que los
;   avisos salgan aunque falte el acceso directo (la app también lo registra al arrancar). Los
;   accesos directos del menú Inicio y del escritorio llevan el mismo AUMID: los crea la plantilla de
;   cargo-packager con `ApplicationID::Set "<.lnk>" "${IDENTIFIER}"`.
;
; Las asociaciones de archivo NO se declaran en `fileAssociations` de cargo-packager: su macro
; escribe el comando sin comillas en la ruta del .exe, se apropia del valor por omisión de `.eml` y
; su desinstalador no las borra (0.11.8). Aquí se hace todo, con comillas, y se deshace al desinstalar.

!define IUREDESK_OPEN "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""
!define IUREDESK_ICON "$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0"
!define IUREDESK_CAPS "Software\Iurefficient\IureDesk\Capabilities"
!define IUREDESK_AUMID "Software\Classes\AppUserModelId\${IDENTIFIER}"

; ProgID que abre un archivo o enlace con `"…\iuredesk.exe" "%1"`.
!macro IureDeskProgId PROGID DESC
  WriteRegStr SHCTX "Software\Classes\${PROGID}" "" "${DESC}"
  WriteRegStr SHCTX "Software\Classes\${PROGID}" "AppUserModelID" "${IDENTIFIER}"
  WriteRegStr SHCTX "Software\Classes\${PROGID}\DefaultIcon" "" "${IUREDESK_ICON}"
  WriteRegStr SHCTX "Software\Classes\${PROGID}\shell\open\command" "" "${IUREDESK_OPEN}"
!macroend

; Tipo de archivo: ProgID propio, «Abrir con» y capacidad para «Aplicaciones predeterminadas».
!macro IureDeskFileType EXT DESC
  !insertmacro IureDeskProgId "IureDesk.${EXT}" "${DESC}"
  WriteRegStr SHCTX "Software\Classes\.${EXT}\OpenWithProgids" "IureDesk.${EXT}" ""
  WriteRegStr SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe\SupportedTypes" ".${EXT}" ""
  WriteRegStr SHCTX "${IUREDESK_CAPS}\FileAssociations" ".${EXT}" "IureDesk.${EXT}"
!macroend

!macro IureDeskFileTypeRemove EXT
  DeleteRegKey SHCTX "Software\Classes\IureDesk.${EXT}"
  DeleteRegValue SHCTX "Software\Classes\.${EXT}\OpenWithProgids" "IureDesk.${EXT}"
  DeleteRegKey /ifempty SHCTX "Software\Classes\.${EXT}\OpenWithProgids"
  DeleteRegKey /ifempty SHCTX "Software\Classes\.${EXT}"
!macroend

Section IureDeskDefaultApps
  ; AppUserModelID: nombre e icono de los avisos de una app sin paquete (documentado por Microsoft).
  WriteRegStr SHCTX "${IUREDESK_AUMID}" "DisplayName" "${PRODUCTNAME}"
  WriteRegStr SHCTX "${IUREDESK_AUMID}" "IconUri" "$INSTDIR\${MAINBINARYNAME}.ico"

  ; Protocolo iuredesk: (clic en una notificación = activación por protocolo).
  WriteRegStr SHCTX "Software\Classes\iuredesk" "" "URL:IureDesk"
  WriteRegStr SHCTX "Software\Classes\iuredesk" "URL Protocol" ""
  WriteRegStr SHCTX "Software\Classes\iuredesk\DefaultIcon" "" "${IUREDESK_ICON}"
  WriteRegStr SHCTX "Software\Classes\iuredesk\shell\open\command" "" "${IUREDESK_OPEN}"

  ; mailto: con su propio ProgID (no se pisa HKCR\mailto de otra aplicación).
  !insertmacro IureDeskProgId "IureDesk.mailto" "URL:MailTo Protocol"
  WriteRegStr SHCTX "Software\Classes\IureDesk.mailto" "URL Protocol" ""

  ; La app en «Abrir con» (nombre visible y comando).
  WriteRegStr SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe" "FriendlyAppName" "${PRODUCTNAME}"
  WriteRegStr SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe\DefaultIcon" "" "${IUREDESK_ICON}"
  WriteRegStr SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe\shell\open\command" "" "${IUREDESK_OPEN}"

  ; Archivos.
  !insertmacro IureDeskFileType "eml" "E-mail message"
  !insertmacro IureDeskFileType "ics" "iCalendar file"
  !insertmacro IureDeskFileType "vcf" "vCard contact"

  ; Capacidades para «Aplicaciones predeterminadas».
  WriteRegStr SHCTX "${IUREDESK_CAPS}" "ApplicationName" "${PRODUCTNAME}"
  WriteRegStr SHCTX "${IUREDESK_CAPS}" "ApplicationDescription" "Mail, calendar, contacts, notes and tasks"
  WriteRegStr SHCTX "${IUREDESK_CAPS}" "ApplicationIcon" "${IUREDESK_ICON}"
  WriteRegStr SHCTX "${IUREDESK_CAPS}\URLAssociations" "mailto" "IureDesk.mailto"
  WriteRegStr SHCTX "Software\RegisteredApplications" "IureDesk" "${IUREDESK_CAPS}"

  ; Avisa al Explorador de que cambiaron las asociaciones (SHCNE_ASSOCCHANGED).
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd

Section un.IureDeskDefaultApps
  DeleteRegValue SHCTX "Software\RegisteredApplications" "IureDesk"
  DeleteRegKey SHCTX "Software\Iurefficient\IureDesk"
  DeleteRegKey /ifempty SHCTX "Software\Iurefficient"

  ; El protocolo solo si sigue apuntando a esta instalación.
  ReadRegStr $R7 SHCTX "Software\Classes\iuredesk\shell\open\command" ""
  ${If} $R7 == "${IUREDESK_OPEN}"
    DeleteRegKey SHCTX "Software\Classes\iuredesk"
  ${EndIf}

  DeleteRegKey SHCTX "Software\Classes\IureDesk.mailto"
  !insertmacro IureDeskFileTypeRemove "eml"
  !insertmacro IureDeskFileTypeRemove "ics"
  !insertmacro IureDeskFileTypeRemove "vcf"
  DeleteRegKey SHCTX "Software\Classes\Applications\${MAINBINARYNAME}.exe"
  DeleteRegKey SHCTX "${IUREDESK_AUMID}"

  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
SectionEnd
