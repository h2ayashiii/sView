; sView の NSIS インストーラーフック。
; tauri.conf.json の bundle.windows.nsis.installerHooks から読み込まれ、
; tauri のテンプレートが生成する installer.nsi に !include される。
;
; 目的: ユーザー単位インストール（%LOCALAPPDATA%\sView）からマシン単位
; インストール（C:\Program Files\sView）へ移行したときの後始末。
; 旧インストールを残したままだと以下の問題が出るため、本体のインストール
; 直前にサイレントアンインストールする。
;   - 「アプリと機能」の項目が二重になる（HKCU 側と HKLM 側の両方に残る）
;   - ファイル関連付けは HKCU が HKLM より優先されるため、画像を開くと
;     古い実行ファイルが起動してしまう
;
; マクロの中身は !insertmacro された場所（installer.nsi 側）で展開される。
; そのため ${UNINSTKEY} などテンプレート側の !define をここで参照できる。
; インストーラー UI 自体は英語なので、表示文字列も英語にしてある。

!macro NSIS_HOOK_PREINSTALL
  Push $0
  Push $1

  ; perMachine では SHCTX が HKLM を指すので、HKCU を明示して読む
  ReadRegStr $0 HKCU "${UNINSTKEY}" "UninstallString"
  ReadRegStr $1 HKCU "${MANUPRODUCTKEY}" ""

  ${If} $0 != ""
  ${AndIf} $1 != ""
  ${AndIf} $1 != $INSTDIR
  ${AndIf} ${FileExists} "$1\uninstall.exe"
    DetailPrint "Removing the previous per-user installation..."
    ; _?= を付けるとアンインストーラーが自身を temp にコピーせず同期実行される。
    ; /S はサイレント実行。設定ファイル（%APPDATA%\sview）は消えない。
    ExecWait '$0 /S _?=$1' $0
    ; _?= 指定時はアンインストーラーが自分自身を消せないので後始末する
    Delete "$1\uninstall.exe"
    RMDir "$1"
  ${EndIf}

  Pop $1
  Pop $0
!macroend
