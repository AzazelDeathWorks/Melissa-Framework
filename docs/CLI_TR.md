# Melissa CLI

`melissa` yazın ve doğrudan konuşun. Tam ekran terminal açılır; sohbet için proje seçmek gerekmez. Bulunulan klasör uygun olduğunda dosya bağlamı olur. Dosya oluşturma veya düzenleme gerektiğinde klasör bağlayabilir ya da yeni klasör oluşturabilirsiniz. Genel sorular dosya ağacını taramaz.

## İlk bağlantı

Sohbet içinde `/config` veya `/settings`, PowerShell'de `melissa config` veya `melissa settings` ayar ekranını açar. JSON dosyası hazırlamak gerekmez. Sağlayıcı eklerken API uyumluluğunu seçin, **Base URL (sunucu adresi)** alanına API adresini yazın; **API anahtarı (API Key)** sonraki ayrı, maskeli alanda sorulur. Ardından model seçilir. Base URL örneği `https://api.example.com/v1`; sağlayıcının hesap/anahtarlar sayfası değildir. Hatalı giriş aynı alanda açıklanır ve kurulumu başa döndürmez. OpenAI Chat Completions, OpenAI Responses ve Anthropic Messages ayrı protokollerdir; tam istek adresi farklı protokole aitse reddedilir.

Anahtar maskelenir ve seçildiğinde Windows kullanıcı anahtarıyla şifreli saklanır. Ortam değişkeni veya dosya referansı da kullanılabilir. Anahtarsız HTTP yalnız açık loopback IP adresleri için desteklenir. `localhost`, yerel bağlantı seçiminde `127.0.0.1` olarak normalleştirilir; uzak HTTP ve yönlendirmeler kabul edilmez.

Sağlayıcı model bilgilerini sunuyorsa `/model` listesinden getirilebilir. Kapasite bilgisi gelmiyorsa modelin belgelenen bağlam ve tam çıktı kapasitesini girin; tahmini bir model sınırı gerçek veri gibi kaydedilmez. Sağlayıcı model listelemeyi desteklemiyorsa kimliği elle ekleyin.

Kaydedilmiş ayar, başarılı model üretimi demek değildir. Bağlantıyı dene seçeneği model listesi erişimini kontrol eder. Araç çağrısı, görsel girdi ve yapılandırılmış çıktı uyumu ayrıca gerçek çağrıyla doğrulanmalıdır.

## Konuşma ve Plan

Normal oturumda soru sorabilir veya yapılacak işi yazabilirsiniz. Salt okuma ve açıklama için hazırlanmış worker veya görev tarifi gerekmez. Model, görünür proje dosyalarını okuyup arayabilir; kabul edilmiş güncel proje belleğini, proje talimatlarını ve eklenti kataloğunu inceleyebilir.

`/plan` veya Shift+Tab, Plan davranışını açar. Plan projenin dosyalarını değiştiremez, silemez, taşıyamaz veya sistem komutu çalıştıramaz. Bu ayrım araç katmanında uygulanır. Model kullanıldığı için planlama da API ücreti doğurabilir.

Normal modda uygulama önerisi hazır olduğunda tek kapsam incelemesine geçilir. Plan modunda uygulama penceresi açılmaz; Shift+Tab ile normal moda dönüp `/apply` kullanabilir veya konuşarak devam edebilirsiniz. Uygulama öncesinde kesin dosya ve kontrol komutu kapsamı incelenir. Onay, o anki kaynak sürümüne bağlıdır; araya giren değişiklik eski onayı geçersiz kılar. Normal kullanıcı `/create`, `/approve`, `/run`, teknik kimlik veya JSON tarifi yazmaz.

## Model ve harcama

`/model` görünür konuşmayı korur. Başka sağlayıcıya geçerken geçmişin gönderilmesi ayrıca sorulur. Sağlayıcıya özel gizli devam/signature verisi aktarılmaz. Görsel desteklemeyen model seçilirse önceki görseller kayıtta kalır ve modele gönderilmez; bu fark ekranda belirtilir.

Harcama tavanı isteğe bağlıdır. Bilinmeyen fiyat veya kullanım sıfır dolar olarak sunulmaz. Bütçeli native yürütme için fiyat bilgisi gerekir; kullanıcı tarafından girilen fiyat, doğrulanmış sağlayıcı faturası diye etiketlenmez. Seçilen modelin çıktı kapasitesi harcama sınırına uydurulmak için azaltılmaz.

API izni, seçili sağlayıcının mesaj ve gerekli görünür kaynak içeriğini alabileceğini ve ücret doğabileceğini açıklar. Bir çağrı yarım kalırsa kayıt korunur; otomatik tekrar yapılmaz. `/resume` belirsiz çağrıyı tekrarlamadan devam etme tercihini ayrıca gösterir; maliyet bilinmiyorsa bilinmiyor kalır.

## Proje çalıştırma ortamı

Modelin kaynak değiştirmesi ve kontrol çalıştırması mevcut native görev motoru, kesin kapsam ve bağımsız doğrulama kurallarını kullanır. Test/build ortamı hazırlanmış izole worker gerektirir. Ayarlardaki Proje ortamı, yerel image'leri listeler ve seçilen tam image kimliğini projeye kaydeder. Herhangi bir image'in görünmesi, gerekli araç ve bağımlılıkların bulunduğunu kanıtlamaz.

Manifestlerden komutlar keşfedilir; yeni proje için önerilen gerçek kontrol komutları kullanıcı incelemesine sunulabilir. Eksik ortam veya bağımlılık başarı sayılmaz. Genel ağdan veya host üzerinde sessiz paket kurulumu yapılmaz. Ayrıntılar: [proje ortamı](PROJECT_RUNTIME_TR.md).

## Günlük komutlar

| Komut | İşlev |
| --- | --- |
| `/help` | Kısa yardım |
| `/config`, `/settings` | Terminal ayarları |
| `/model`, `/models` | Model seçimi |
| `/plan` | Değişiklik yapmadan planlama |
| `/new` | Kayıtları koruyarak yeni konuşma |
| `/resume` | Aranabilir konuşma geçmişi; varsa duraklatılmış çalışmaya dönüş |
| `/diff` | Konuşmada uygulanan görevin kod farkı |
| `/undo` | Makbuz, dosya ve parça seçerek önizlemeli geri alma |
| `/compact` | Özgün geçmişi silmeden model bağlamını özetle |
| `/search` | Konuşmada ara; F3 sonraki eşleşme |
| `/transcript` | Tam konuşma ve araç sonuçları |
| `/queue` | Bekleyen mesajı incele veya taslağa geri al |
| `/apply` | Plan modundan çıktıktan sonra mevcut önerinin kapsamını incele |
| `/project` | Proje seçme/oluşturma |
| `/exit` | Çıkış |

Enter gönderir; Shift+Enter, Alt+Enter veya Ctrl+J yeni satır ekler. Tab komut veya `@dosya` seçimini açar. Boşluklu yol için `@"src/my file.rs"` kullanılabilir. Metin ve desteklenen modellerde PNG ekleri aynı proje görünürlük kurallarına tabidir. Ctrl+R mesaj geçmişini arar; ok tuşları önceki mesajları getirir. Ctrl+V/Ctrl+Y, Windows panosundan yapıştırır. Esc veya Ctrl+C etkin işi durdurmayı ister; duruş doğrulanmadan yeni yetki verilmez.

Konuşmalar, taslaklar ve model çağrısı kayıtları kullanıcı veri klasöründe şifreli tutulur. Kaynak kontrolüne eklenmez. Geri alma yalnız bilinen native dosya etkilerini kapsar; API, veritabanı veya Git geçmişini geri alma garantisi değildir.

Teknik otomasyon arayüzü korunur: `melissa --help-all`. Eski görev ve kanıtlar, yeni terminal görünümü nedeniyle yeniden yazılmaz.

## Tasarım referansı

Terminalin sade konuşma alanı, durum satırı ve aranabilir seçicileri için MIT lisanslı [Oh My Pi](https://github.com/can1357/oh-my-pi/tree/79fa222e8390aed3494f4bc6746814e6c74c21cf/packages/tui/src) incelendi. Rust uygulaması mevcut Ratatui altyapısını kullanır; OMP'nin bütün özellikleriyle eşitlik veya ölçülmüş kalite üstünlüğü iddia edilmez.

## Tam ekran ve süreklilik

PgUp/PgDn veya tekerlek geçmişi kaydırır. Ctrl+End son mesaja döner; geçmiş okunurken yeni çıktı ekranı aşağı çekmez. Ctrl+F arar, F3 sonraki eşleşmeye gider. Ctrl+O araç ayrıntılarını açar. `/diff` kaydırılabilir inceleme ekranıdır. Ayar pencerelerinden Esc ile dönülür; taslak korunur.

Yanıt gelirken Enter yeni mesajı sıraya alır. Kod çalışırken normal mesaj mevcut işe yönlendirme olarak iletilir; Ctrl+Q sonraki işe bırakır. Kuyruk şifreli saklanır ve yeniden açılışta kendiliğinden ücretli çağrı başlatmaz.

Bağlam dolmaya yaklaşınca eski konuşma modelle özetlenebilir. `/compact` aynı işlemi elle başlatır ve API kullanır. Son istek ve tam özgün kayıt korunur; model eski ayrıntıları `conversation_history` ile sayfalayabilir. Özet tamamlanmazsa bağlam değiştirilmez. Tek başına çok büyük bir ek sessizce kesilmez.

Kesin, yazmasız kaynak görünürlüğü retleri bağımsız izinli çalışmayı engellemez. Ret, gizli dosyanın varlığını veya içeriğini açıklamaz; reddedilen erişim için alternatif yol yetkisi oluşturmaz. Yetki kapsamı, yazma sonrası belirsizlik ve kalıcı kayıt hataları güvenli duruş gerektirmeye devam eder.
