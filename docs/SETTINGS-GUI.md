# 詞庫圖示與設定

VoiceType提供獨立GTK3視窗與AppIndicator圖示，管理`~/.config/voicetype/vocab.toml`。儲存後下一句自動重載；圖示不負責錄音，退出圖示仍可聽寫。

## 安裝

需要Python 3.11+、OpenCC，以及Debian/Ubuntu套件：

```sh
sudo apt install python3-venv python3-gi python3-gi-cairo \
  gir1.2-gtk-3.0 gir1.2-ayatanaappindicator3-0.1 gir1.2-gstreamer-1.0 \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good
bash scripts/install-settings.sh
~/.local/bin/voicetype-settings
```

桌面需支援StatusNotifierItem，例如GNOME AppIndicators擴充。安裝器建立內容定址的Python runtime，固定tomlkit版本，新增應用程式入口及首次登入啟動設定；不重啟Fcitx或替換daemon。`voicetype-settings --tray`只顯示圖示。再次安裝保留登入啟動選擇。

## 編輯詞庫

點圖示 →「VoiceType 詞庫與設定…」，選現有詞或按「＋ 新增詞彙」。正確寫法填`GitHub`，錯法每行一個，例如`gthub`、`git hub`，按儲存即可。內建範例只填表單，儲存前不生效。「名字保護」可固定專名字形。

「試打看看」走正式daemon文字管線，不錄音、不插入別的視窗、不產生學習樣本。試的是已儲存詞庫；有草稿時先儲存。無效TOML、衝突詞對和外部編輯會顯示錯誤。保存保留註解與未知欄位，以原子替換寫入，並留0600的`vocab.toml.bak`；外部編輯保護是內容比對，不宣稱能鎖住所有編輯器。

視窗關閉只隱藏，草稿保留；退出或切換未儲存編輯時會確認。「設定與說明」提供登入自動顯示與上一版回復。

「待校對」可重聽抽樣、確認或修改結果，再明確加入詞庫，見[抽樣校對](REVIEW-QUEUE.md)。這需要包含review收集器的daemon，僅安裝GUI不會替舊daemon增加功能。

GUI不直接編輯daemon管理的`personalization.json`。已學習規則請用`voicetype-control learned`／`forget`管理，避免繞過daemon寫入邊界。
