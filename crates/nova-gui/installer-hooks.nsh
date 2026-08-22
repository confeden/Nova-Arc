; Explorer's context menu, added on install and removed on uninstall.
;
; A CASCADING menu, built entirely from the registry: one "Nova Prism" entry
; that opens into verbs. `ExtendedSubCommandsKey` is what makes it cascade
; without a COM handler — the alternative is an IExplorerCommand DLL, which is
; also the only way onto Windows 11's *short* menu. Here the entry lands in the
; short menu on Windows 10 and under "Show more options" on Windows 11, which
; is where 7-Zip and WinRAR live too until they ship a packaged handler.
;
; Everything is written to SHCTX, the root Tauri's own installer already chose:
; HKLM for a machine-wide install, HKCU for a per-user one. Writing HKLM
; unconditionally would need admin rights the per-user path does not have.
;
; TWO MENUS, because the verbs differ:
;   * and Directory  -> "Сжать" (anything can be compressed)
;   archive types    -> open / extract / test
;
; The archive menu is attached to nova's own ProgID and to
; SystemFileAssociations for zip, 7z and rar — that last key adds a verb to a
; file type WITHOUT taking its association away, so whatever opens .zip today
; keeps opening it.

!macro NOVA_MENU_ROOT KEY TITLE SUBKEY
  WriteRegStr SHCTX "Software\Classes\${KEY}\shell\NovaPrism" "MUIVerb" "${TITLE}"
  WriteRegStr SHCTX "Software\Classes\${KEY}\shell\NovaPrism" "Icon" "$INSTDIR\${MAINBINARYNAME}.exe,0"
  WriteRegStr SHCTX "Software\Classes\${KEY}\shell\NovaPrism" "ExtendedSubCommandsKey" "Software\Classes\${SUBKEY}"
!macroend

!macro NOVA_MENU_ITEM SUBKEY ORDER LABEL ARGS
  WriteRegStr SHCTX "Software\Classes\${SUBKEY}\shell\${ORDER}" "MUIVerb" "${LABEL}"
  WriteRegStr SHCTX "Software\Classes\${SUBKEY}\shell\${ORDER}" "Icon" "$INSTDIR\${MAINBINARYNAME}.exe,0"
  WriteRegStr SHCTX "Software\Classes\${SUBKEY}\shell\${ORDER}\command" "" '"$INSTDIR\${MAINBINARYNAME}.exe" ${ARGS} "%1"'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; --- what can be compressed: any file, any folder, and a folder's background
  !insertmacro NOVA_MENU_ROOT "*" "Nova Prism" "NovaPrism.Pack"
  !insertmacro NOVA_MENU_ROOT "Directory" "Nova Prism" "NovaPrism.Pack"
  !insertmacro NOVA_MENU_ITEM "NovaPrism.Pack" "10pack" "Сжать в Nova Prism" "--compress"

  ; --- what can be opened: our own archives and the foreign ones we read
  !insertmacro NOVA_MENU_ROOT "NovaPrismArchive" "Nova Prism" "NovaPrism.Archive"
  !insertmacro NOVA_MENU_ROOT "SystemFileAssociations\.zip" "Nova Prism" "NovaPrism.Archive"
  !insertmacro NOVA_MENU_ROOT "SystemFileAssociations\.7z" "Nova Prism" "NovaPrism.Archive"
  !insertmacro NOVA_MENU_ROOT "SystemFileAssociations\.rar" "Nova Prism" "NovaPrism.Archive"

  ; Order matters: this is the order they appear in. Open first because it is
  ; the one people reach for; test last because it is the rare one.
  !insertmacro NOVA_MENU_ITEM "NovaPrism.Archive" "10open" "Открыть" ""
  !insertmacro NOVA_MENU_ITEM "NovaPrism.Archive" "20into" "Распаковать в отдельную папку" "--extract-into"
  !insertmacro NOVA_MENU_ITEM "NovaPrism.Archive" "30here" "Распаковать здесь" "--extract-here"
  !insertmacro NOVA_MENU_ITEM "NovaPrism.Archive" "40test" "Проверить" "--test"

  ; Explorer caches the menu; without this the entry appears only after a
  ; restart, which reads as "the installer did not work".
  System::Call 'shell32::SHChangeNotify(i 0x8000000, i 0, i 0, i 0)'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegKey SHCTX "Software\Classes\*\shell\NovaPrism"
  DeleteRegKey SHCTX "Software\Classes\Directory\shell\NovaPrism"
  DeleteRegKey SHCTX "Software\Classes\NovaPrismArchive\shell\NovaPrism"
  DeleteRegKey SHCTX "Software\Classes\SystemFileAssociations\.zip\shell\NovaPrism"
  DeleteRegKey SHCTX "Software\Classes\SystemFileAssociations\.7z\shell\NovaPrism"
  DeleteRegKey SHCTX "Software\Classes\SystemFileAssociations\.rar\shell\NovaPrism"
  DeleteRegKey SHCTX "Software\Classes\NovaPrism.Pack"
  DeleteRegKey SHCTX "Software\Classes\NovaPrism.Archive"
  System::Call 'shell32::SHChangeNotify(i 0x8000000, i 0, i 0, i 0)'
!macroend
