# Proje servisleri, API ve migration provası

Bu katman genel CLI projesinin incelenmiş servislerini gerçekten çalıştırır.
API ile ayrı veritabanı fixture'ı birlikte hazır olur; log, SQL, sözleşme testi ve
migration kanıtları görev/kaynak/ortam sürümüne bağlanır. Modelin başarı beyanı
görev kabulü yerine geçmez. Canlı model kalitesi ve yayın kabulü ayrıca ölçülür.

## Ortam ve yeni görev

[Servis araç imajı](../scripts/toolchains/services/README.md) mevcut web/SQL SDK'sına
Schemathesis ekler. Yeni görevi hazırlamadan önce bu imajın incelenmiş kimliğini
seçin. [Örnek ayarı](../examples/cli/project-services.template.json) kendi gerçek
dosya yollarınıza, komutlarınıza ve kontrol adlarınıza göre düzenleyin:

```text
/environment sha256:REVIEWED_IMAGE_ID
/check {"executable":"python","argv":["-I","-m","unittest","discover"],"cwd":"."}
/services C:/Private/project-services.json
```

İlk ek `/check`, `operator-1` adını alır. Başka ek kontroller varsa gerçek adı
`/details` üzerinden seçin. Servise bağımlı proje kontrollerini `business_checks`
listesine yazın. Genel `verify_change` bu kontrolleri servis olmadan çalıştırmaz;
`project_services` içindeki `business` veya aşağıdaki `services run` kullanılır.
Servisten bağımsız build/lint/format/test kontrolleri önceki doğrulama yolunu korur.

```powershell
melissa --json services configure --workspace C:/Project --path C:/Private/project-services.json
melissa --json services show --workspace C:/Project
```

Tercihler proje kök kimliğine bağlı şifreli saklanır ve yalnız yeni göreve
uygulanır. Görev onayında tam sözleşme sabitlenir. Sonraki ayar mevcut görevin
servislerini, komutlarını, bütçesini veya son tarihini değiştirmez. Komutlar
kaynakta keşfedilmiş ya da operatörce açıkça eklenmiş olmalıdır.

## Servis oturumu

```powershell
./scripts/service-session.ps1 -TaskId TASK
```

Bu bir ön plan oturumudur; çalışırken JSON komutları gönderilir:

```json
{"operation":"status"}
{"operation":"api","service":"api","request":{"session":"client","method":"get","path":"/health","headers":{},"body":null,"timeout_ms":3000}}
{"operation":"database","command":{"command":"query","connection":"read","sql":"SELECT id,name FROM items ORDER BY id","parameters":[]}}
{"operation":"log","service":"api","stream":"stderr","offset":0,"max_bytes":32768}
{"operation":"business","name":"operator-1"}
```

PostgreSQL servis logu aynı log komutunda `service:"database"`, `stream:"stderr"`
ile okunur. SQLite daemon logu yerine SQL işlem/fixture kanıtları bulunur.

`/close` veya girişin kapanması oturumu kapatır. Doğrudan CLI:
`melissa --json services session TASK`. Servisler aynı görevdeki API/DB/yardımcı
süreçlerdir; bağımlılık sırasıyla başlar, yalnız HTTP portunun açılması yerine
incelenmiş health yolunun beklenen durum kodu aranır. Döngü ve port çakışması
başlamadan reddedilir. Son durum ayrıca kontrol edilir; hazır olma kaydı süresiz
sağlık garantisi değildir. Başlangıç hatasının stdout/stderr'i kaybedilmez. Normal kapanışta tam servis
günlükleri özel close detayına alınır; kapasiteye sığmazsa temizlendi diye tam
günlük varmış gibi gösterilmez ve kayıt engeli açık kalır.

Model `project_services` aracını kullanır: `command.operation=start` ve null
`session_id`, sonra dönen kesin oturum kimliği. `service_result_page`, yalnız
aynı görevin service receipt/detail kayıtlarını byte sayfalarıyla okur. Ham
API cevabı ve loglar kısa yanıtta bulunmaz; `detail_artifact` ile tam okunur.
Bunlar özel proje verisi ve güvenilmeyen çıktıdır; talimat veya izin değildir.

Komutlar hazır kaynak kopyasını kullanır. Kaynak değişince eski servis yeni
kaynağı doğrulayamaz: oturum kapatılıp yeniden açılır. Eski log/status gözlemi
okunabilir ve `source_current=false` ile etiketlenir. Ajan, düzenleme ve teslim
öncesinde sahip olduğu oturumu kapatır. İş bitişinde host da native duruşu
bağımsız doğrular. Eksik intent veya belirsiz yazım otomatik tekrarlanmaz.

## İzolasyon ve Compose

`backend` değeri `docker` veya `compose` olabilir. Compose seçimi imzalı native
isteğe dahildir; izin alındıktan sonra değiştirilemez. İki backend de aynı
read-only root/tmpfs, kullanıcı, CPU/RAM/disk/PID, bağımsız süre ve çıktı
kontrollerini kullanır; native denetim gerçek kernel ayarlarını karşılaştırır.

Çok sayıda mantıksal servis, **bir göreve ait tek izole kapsülde** yönetilir.
Compose, bu kapsülü yürütücünün ürettiği sonlu manifestten oluşturur; kullanıcının
rastgele compose.yaml dosyasını daemon yetkisiyle çalıştırmaz. API/DB süreçleri
kapsül içindeki loopback veya özel Unix socket üzerinden haberleşir. Host port,
host kaynak/volume mount, Docker socket, harici ağ veya üretim bağlantısı açılmaz.
Farklı image gerektiren bileşenler için uygun birleşik SDK hazırlanmalıdır;
rastgele çok image'li Compose dağıtımı bu bağlantının kapsamında değildir.

Docker CLI ve Compose plugin'i host tarafından seçilir. Docker Desktop'ın
hardlink'li plugin'i salt okunur tutulup özel alana birebir kopyalanır; çalışacak
tek linkli kopyanın özeti ve değiştirilemez dosya tutamacı doğrulanır. Worker
Docker yetkisi almaz. İptal yalnız kesin native container kimliği, nonce ve image
bağına dokunur; genel `compose down`, volume silme veya orphan temizliği yapılmaz.
Tarayıcı/canlı önizleme erişimi ayrı bağlantıdır; guest origin host URL değildir.

## API sözleşmesi ve iş kuralları

```powershell
./scripts/api-contract-check.ps1 -TaskId TASK -Name api_contract
melissa --json services run TASK --request-json '{"operation":"business","name":"operator-1"}'
```

Her tek adım, ayrı taze servis ortamını açar, işlemi çalıştırır, kapatıp aynı
görevi güvenli pause'a getirir. `/resume` model akışını ayrıca sürdürür.
`-Melissa` güncel exe, `-MelissaHome` ayrı özel görev alanı seçebilir.

Schemathesis yalnız kayıtlı kaynak şeması ve kayıtlı servis origin'iyle çalışır.
Pozitif/negatif veri, examples/coverage/fuzzing/stateful aşamaları ve seed kayda
geçer. `examples`, operatörün test başına örnek tercihidir; model çıktı veya araç
çağrısı sınırı değildir. Native görev kaynak bütçesi her zaman ayrıca uygulanır.
Sözleşme testi fixture verisini değiştirebilir; sonuç farklı veri durumunda tekrar
edildiğinde aynı sonuç varsayılmaz. Taze başlangıç, kaynak/ortam ve seed korunur.

JUnit sayıları **API operasyonlarını/aşama gruplarını** sayar; üretilen bütün HTTP
isteklerinin sayısı değildir. Tam başarısızlık metni, küçük örnek/curl ve seed
özel detayda kalır. Report/tool yoksa BLOCKED; sıfır/okunamayan envanter PASS
olmaz. İş kuralı kontrolü ayrı onaylı proje komutudur. Standart unittest/pytest/
Cargo/TAP veya tanımlı JUnit sayıları ve tekrarlar önceki doğrulama toplayıcısıyla
korunur. Retry/tekrar hataları temiz PASS'e indirgenmez.

Sözleşme uyumu, yetkilendirme ve iş mantığının tamamı demek değildir. Normal
regresyonlar gerçek yetki ve veri kurallarını ayrıca sınamalıdır.

## Migration provası ve örnek veri

```powershell
./scripts/migration-rehearsal.ps1 -TaskId TASK -Name upgrade
```

`rehearsals` içindeki her kayıt için servis DB'sinden ayrı, atılabilir DB'ler:

1. `fresh`: boş DB üzerinde güncel kurulum; `fresh_sample` (yoksa `sample`) ve `after` sorguları.
2. `baseline` + `sample`: eski sürüm; `before` sorguları.
3. `upgrade`: eski sürümden yükseltme; `after`, şema ve veri özeti.
4. Fresh/upgrade şema eşitliği; varsa `downgrade` sonrası eski şema, veri ve
   invariant'ların gerçekten geri gelmesi.

Veri biçimi dönüşüyorsa `sample` eski sürüme, isteğe bağlı `fresh_sample` güncel
sürüme uygun sentetik veriyi seçer. Böylece veri dönüşümü yükseltmede gerçekten
çalışır; temiz kurulum için eski biçim zorlanmaz.

SQLite ve PostgreSQL desteklenir. SQL dosyaları görünür kaynakta olmalıdır.
Transaction kontrolü host'a aittir; script içi BEGIN/COMMIT/ROLLBACK ve benzeri
kontroller reddedilir. SQL yazımı modelden ham SQL yerine kayıtlı statement
adıyla seçilir; sorgular mevcut read-only SQL kapısından geçer.

`transactional=true` hata durumunda rollback kanıtını tutar. Transaction dışındaki
kısmi değişiklik FAIL olarak kalır; geri alınmış gibi gösterilmez. `downgrade:null`
ise rollback `NOT_SUPPORTED`; tanımlı ancak çalıştırılmamışsa `NOT_RUN` olur.
Bunlar birbirinin yerine kullanılmaz. Bu kontrol yalnız tanımlı invariant ve
mantıksal şema/veri özetini kapsar; tüm üretim verisinin eksiksiz provası değildir.

`provenance` yalnız `synthetic` veya `anonymized` olabilir. Bu, operatörün kaynak
veri beyanıdır; otomatik anonimleştirme iddiası değildir. Üretim DSN/host/şifre
alanı yoktur. PostgreSQL'in fiziksel geçici prova dosyaları mantıksal kanıt ve
loglar alındıktan sonra kendi worker alanından kaldırılır; asıl proje, servis
DB'si, başka oturum veya eski test kayıtları değişmez.

## Son kabul ve hata sınıfları

Final host, taze servisler üzerinde `business_checks` ile açık `acceptance`
listesindeki contract/migration kontrollerini yeniden çalıştırır. Diğer onaylı
proje kontrolleri ayrıca çalışır. Son kaynak/ortam ve native duruş kanıtı gerekir.
Bu public proje kabulüdür; özel oracle veya yayın kabulü değildir.

FAIL, FLAKY, NOT_RUN, BLOCKED ve INCONCLUSIVE ayrı tutulur. Sonuç belirsizse
`requires_reconciliation=true` otomatik tekrarı durdurur. API read hatası başarı
sayılmaz; cevabı belirsiz yazım tekrar gönderilmez. CLI `services run/session`
sonucu PASS değilse JSON korunduktan sonra çıkış kodu 7 olur. API/model hiç
çağrılmadıysa model testi başarısız olmuş gibi gösterilmez.

## Geliştirici doğrulaması

Rust normal testleri, explicitly çalıştırılan Windows/Docker servis testleri,
Testcontainers testi ve canlı model kabulü farklı kanıtlardır:

```powershell
cargo build --locked -p melissa-cli
$env:MELISSA_PHASE07_IMAGE='sha256:REVIEWED_IMAGE_ID'
$env:MELISSA_PHASE07_CLI='C:/Build/target/debug/melissa.exe'
cargo test --locked -p melissa-cli --lib services::tests::native_ -- --ignored --test-threads=1
```

[Testcontainers geliştirici testi](../scripts/tests/services_testcontainers.py)
mevcut değişmez SDK üzerinde iki ayrı PostgreSQL açar. Hatalı migration rollback'i
ve hedef temizlenirken diğer DB'nin çalışması gerçekten kontrol edilir. Host'ta
ayrı özel Python ortamı ve [Windows lock dosyası](../scripts/tests/services-testcontainers-windows.lock)
kullanılır; bu Docker SDK erişimi model araçlarına veya worker'a verilmez. Ryuk
kapalıdır; test yalnız kendi kesin container nesnelerini finally içinde kaldırır.
