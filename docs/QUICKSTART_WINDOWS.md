# Windows'ta Melissa

Windows x64 önizlemesidir. Güncel yerel aday: **0.3.0-alpha.6**. Bu belge, bu adayın GitHub'da yayımlandığı anlamına gelmez.

## Kurulum

Güvendiğiniz Melissa paket klasöründe `install.ps1`, sürüm ZIP'i ve `SHA256SUMS.txt` birlikte bulunur. PowerShell'de:

```powershell
.\install.ps1
```

Kurucu kullanıcı hesabına kurar; yönetici yetkisi istemez. Yeni bir terminal açın:

```powershell
melissa
```

Sohbet doğrudan açılır; proje seçimi zorunlu değildir. Dosyalarla çalışırken gerektiğinde klasör bağlanır. Başka bir klasörü açıkça seçmek için:

```powershell
melissa session --workspace C:\Projects\example
```

Dağıtımda bir masaüstü GUI bulunmaz. CLI'ın kurulması için Tauri, WebView veya bir Node frontend kurulumu gerekmez. Kod testleri için gereken proje araçları/worker ayrı bir konudur.

## İlk kullanım

1. Sohbet içinde `/settings` açın veya PowerShell'de `melissa config` yazın; sağlayıcı ekleyin. JSON dosyası hazırlamak gerekmez.
2. API uyumluluğunu seçin. **Base URL** alanına sağlayıcının sunucu adresini yazın (örnek: `https://api.example.com/v1`). **API anahtarı** sonraki ayrı, maskeli alanda sorulur. Ardından modeli seçin. Model bilgisini sağlayıcı sunmuyorsa belgelenen tam kapasiteyi belirtin.
3. API kullanım tercihine karar verin. Bağlantı kontrolü ile gerçek model çağrısı aynı kabul değildir.
4. İsteğinizi yazın: örneğin “Bu projeyi açıkla” veya “Giriş ekranındaki hatayı düzelt”.
5. Değişiklikten önce kesin kapsamı inceleyin. Normal kullanıcı görev tarifi veya JSON hazırlamaz.

Planlamak için `/plan` veya Shift+Tab kullanın. Plan kaynak dosyalarını değiştiremez. Shift+Tab ile normal moda dönünce çalışma kapsamı incelenebilir; Plan açıkken uygulama başlatılmaz.

`/model` modelleri, `/resume` konuşmaları, `/diff` kod farkını ve `/undo` kontrollü geri almayı açar. Ayrıntılı [terminal rehberi](CLI_TR.md).

## Sorun giderme

- Komut bulunamıyorsa kurulumdan sonra yeni terminal açın. Çalışan terminalin PATH'i eski olabilir.
- Model ayarı kaydedildiyse fakat bağlantı çalışmıyorsa `/config` içinden adres, protokol ve kimlik bilgisini kontrol edin. Anahtarı hata raporuna yapıştırmayın.
- Kod çalıştırma ortamı eksikse Docker Desktop'ın Linux motorunu ve sürümle eşleşen hazırlanmış worker'ı kontrol edin. Proje ortamı ayarlardan seçilir; rastgele bir image'in seçilmesi SDK uygunluğunu kanıtlamaz.
- Bozuk veya yarım kayıtları silmeyin. Belirsiz model çağrısı otomatik tekrarlanmaz; `/resume` kayıtları koruyarak devam tercihini gösterir.

Teknik otomasyon komutları `melissa --help-all` içindedir. Yerel test, gerçek model kalitesi veya temiz Windows kabulü demek değildir.
