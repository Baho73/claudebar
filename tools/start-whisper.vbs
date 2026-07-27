' start-whisper.vbs — тихий запуск start-whisper.ps1 из автозагрузки Windows.
' Нужен только чтобы не мигало окно консоли при входе в систему (0 = скрыто, False = не ждать).
' Ставится ярлыком/копией в: %APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup
' Снять автозапуск: удалить оттуда start-whisper.vbs — больше ничего чистить не нужно.
Dim shell, script
script = "D:\Python\claudebar\tools\start-whisper.ps1"
Set shell = CreateObject("WScript.Shell")
shell.Run "powershell -NoProfile -ExecutionPolicy Bypass -File """ & script & """", 0, False
