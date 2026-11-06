# Melissa Framework

Rust tabanlı, geliştirme aşamasındaki ajan ve araç orkestrasyonu çatısı. Proje katkıya açıktır; tamamlanmış ürün veya üretim sürümü olarak sunulmaz.

Genel ajan/worker tanımları, skill paketleri, tipli sözleşmeler, kalıcılık, izin denetimi, araç bağlantıları, iş akışı ve hata toparlama kodları bulunur. Katalogdaki tanımlar, bütün ajanların aynı anda çalıştığı anlamına gelmez.

## CLI — 0.3.0-alpha.3

Windows x64 önizlemesi için [kurulum ve ilk görev rehberi](docs/QUICKSTART_WINDOWS.md).
Paketin yanındaki `install.ps1` tek komutla kullanıcı hesabına kurar. Ardından proje klasöründe `melissa` terminal oturumunu açar.
Oturumda proje paneli, okunabilir görev kartları, Unicode metin düzenleme, ok tuşlarıyla geçmiş ve Tab ile komut tamamlama bulunur. `/details` kayda bağlı tam kapsamı gösterir.
Plan/inceleme/geliştirme kipleri, kapsam farkları ve çalışan görevi yönlendirme için [konuşmalı oturum rehberi](docs/CONVERSATION_TR.md).
Kullanıcı/proje skill'leri, onaylı MCP araçları, sürümlü dokümanlar ve hook'lar için [eklenti rehberi](docs/EXTENSIONS_TR.md).
Güvenlik bildirimleri için [gizli bildirim yolu](SECURITY.md).

Aktif ürün kapsamı 6 Core ve 12 Code profilidir. Diğer alanların genel tanımları
korunur; derlenen CLI'ya dahil edilmez. Masaüstü arayüzü bu kapsamda yoktur.

- Proje bazlı şifreli bellek, kaynak yeniliği denetimi ve kullanıcı onaylı kayıtlar.
- Skill paketleri, bağlam araçları ve izole yardımcı script çalıştırma.
- Gerçek dil sunucusu sorguları ve dosya değişikliğinden sonra erken kontroller.
- Kaynak envanterine dayalı, incelenebilir doğal dil görev taslakları.
- Build/test/lint, sahip olunan hata ayıklama oturumu ve izole Git işlemleri.
- Kalıcı oturum geçmişi, devam etme ve kalan adımları yeniden planlama.
- Göreve göre sıralı uzman seçimi, kaynak sürümüne bağlı devir ve yürütücünün doğruladığı model plan önerileri.

Desteklenen diller, çalışma sınırları ve komutlar:
[CLI kullanım rehberi](docs/CLI_CAPABILITIES_TR.md).
Görev taslağı yerel kurallarla hazırlanır. Özel tarif olmadan proje hazırlık
görevi kaydedilebilir. Model ayarı, harcama tercihi ve incelenmiş ortamla
[genel proje yürütmesi](docs/PROJECT_RUNTIME_TR.md) de açılır: süreç oturumları,
stdin/stdout/stderr, sürüm seçimi ve kilitli bağımlılık kontrolleri. Mevcut özel kabul tarifli yürütme ayrıca kullanıcı incelemesi
gerektirir. Yeni bağlantılar için yerel testler çalıştırılmıştır; canlı model
başarısı veya tamamlanmış üretim sürümü iddiası yoktur.

Kendi bağlantı ayarlarınızı [örnek yapılandırmadan](examples/cli/settings.template.json)
hazırlayın; anahtarınızı ortam değişkeninde veya proje dışındaki özel dosyada tutun.

## Geliştirme

Rust ve Cargo gerekir. İşletim sistemi ve izole çalışma ortamına bağlı testlerin ek gereksinimleri vardır. Başlangıç kontrolleri:

İzole test ortamını hazırlamak için [worker imajı ve yardımcı program kurulumu](scripts/toolchains/README.md) belgesini kullanın.

```text
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked -p melissa-core-contracts -p melissa-local-store
cargo test --locked -p melissa-model-gateway --test reviewed_origin_policy
cargo test --locked -p melissa-cli
```

Katalogdaki backlog verileri şema ve sözleşme testleri için sentetik örneklerdir; proje görev listesi veya ilerleme raporu değildir.

## Katkı

Issue açabilir veya fork üzerinden PR gönderebilirsiniz. PR açıklamasında değişen davranışı, gerekçesini ve çalıştırılan kontrolleri yazın. Mümkün olduğunda tek konuya odaklanın; mevcut güvenlik ve hata durumu kontrollerini koruyun. Genel ajan/skill değişikliklerinde paket bütünlüğünü ve sürüm bağlarını doğrulayın.

Gizli anahtar, erişim bilgisi, kişisel yapılandırma, yerel veri tabanı, çalışma kaydı veya özel dosyaları commit etmeyin. Güvenlik açığı bildirirken çalışır erişim bilgilerini issue veya PR içine koymayın.

MIT lisansı: [LICENSE](LICENSE).

Büyük depolarda kaynak aktarımı, semantik gezinti ve yapısal değişiklik için [depo araçları kılavuzuna](docs/REPOSITORY_SCALE_TR.md) bakın.

Proje kontrolleri, erken doğrulama, gerçek proje debug ayarı ve hata tekrar üretimi: [doğrulama rehberi](docs/VERIFICATION_TR.md).
