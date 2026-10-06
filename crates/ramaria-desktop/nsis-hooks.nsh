; crates/ramaria-desktop/nsis-hooks.nsh - NSIS 安装器钩子（通知应用身份注册）
;
; 设计特点:
; - 安装完成时把应用标识（AUMID）写入当前用户注册表，供系统通知解析应用身份
; - 安装器另行把同一标识写入开始菜单与桌面快捷方式的属性，两者互为兜底
;   （不经快捷方式直接启动时可执行文件时，注册表键仍能解析通知身份）
; - 卸载时移除该键，不留残留
;
; 说明: 宏由 Tauri 生成的 installer.nsi 在安装 / 卸载流程中按名插入；
;       ${BUNDLEID} / ${PRODUCTNAME} 由安装脚本头部定义（与 tauri.conf.json 同源）。

!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "Software\Classes\AppUserModelId\${BUNDLEID}" "DisplayName" "${PRODUCTNAME}"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegKey HKCU "Software\Classes\AppUserModelId\${BUNDLEID}"
!macroend
