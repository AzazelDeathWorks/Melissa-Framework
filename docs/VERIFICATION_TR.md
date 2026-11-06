# Proje doğrulama ve hata tekrar üretimi

Bu katman kullanıcının projesinin gerçek kontrollerini çalıştırır. Harness'in
kendi Rust testleri ve canlı model kalite kabulü ayrı kayıtlardır. Bir proje
kontrolü PASS olduğunda görev otomatik kabul edilmez; gizli oracle veya bütün
özelliklerin kapsandığı sonucu çıkarılmaz.

## Onaydan önce ayarlamak

`/project-doctor` keşfedilen komutları gösterir. `verification` ayarı bu komutların
ne tür sonuç ürettiğini, düzenlemeden sonra hangilerinin erken çalışacağını ve
hata üretme seçimini tanımlar. Komut çalıştırma izni vermez. Görev onayı komut,
kaynak kapsamı, ortam ve bu tercihi birlikte sabitler. Sonraki ayar eski görevi
değiştirmez; bilinmeyen kontrol adı görev oluşturulurken reddedilir.

```powershell
melissa --json verification configure --workspace C:/Project --path C:/Private/verification.json
melissa --json verification show --workspace C:/Project
```

Terminalde `/verification` ve `/verification C:/Private/verification.json` aynı
tercihi kullanır. [Örnek ayar](../examples/cli/project-verification.template.json),
aşağıdaki iki açık kontrolün ilk kez `/check` ile eklendiğini varsayar:

```text
/check {"executable":"python","argv":["-I","-m","py_compile","main.py"],"cwd":"."}
/check {"executable":"python","argv":["-I","-m","pytest","tests","--junitxml=reports/tests.xml"],"cwd":"."}
/verification C:/Private/verification.json
```

Bu iki komut `operator-1` ve `operator-2` adını alır. Diğer komutlar varsa isimleri
`/details` üzerinden kontrol edin. Pytest import düzeni gerekiyorsa projenin
pytest yapılandırmasında `pythonpath` ayarlanır; helper host PYTHONPATH'ini almaz.

Yeni keşfedilmiş type/lint/format kontrolleri, varsa erken kontrol listesine
alınır. Python/Rust için ek hızlı syntax veya format kontrolü açıkça seçilebilir.
Ruff, ESLint, Prettier, derleyici ve test komutları projeye uygun onaylı argv ile
çalışır; format kontrolünde dosya yazan komut yerine check kipi seçin. Her
başarılı dosya mutasyonundan sonra mevcut gramer denetimi ve onaylı `after_edit`
listesi çalışır. Erken kontrol başarısızsa değişiklik uygulanmış olarak kalır;
ajan tanıyı okuyup yeni kaynak sürümüne göre düzeltir, eski patch'i tekrarlamaz.

## Hatadan düzeltmeye

Henüz başlamamış onaylı genel proje görevi veya güvenli duraklatılmış aynı görev:

```powershell
./scripts/reproduce-failure.ps1 -TaskId TASK -Checks operator-2
./scripts/verify-change.ps1 -TaskId TASK -Checks operator-2 -Baseline 'verification:RECEIPT_ID'
```

İkinci komut düzeltmeden sonra çalıştırılır; gerçek baseline kimliğini ilk
çıktının `result.receipt_id` alanından alın. `-Melissa` özel executable,
`-MelissaHome` ayrı özel veri alanı seçer. Doğrudan CLI karşılıkları:

```powershell
melissa --json verification reproduce TASK --request-json '{"names":["operator-2"]}'
melissa --json verification verify TASK --request-json '{"names":["operator-2"],"baseline":"verification:RECEIPT_ID"}'
```

`names` boşsa tüm onaylı kontroller seçilir. `reproduce` tanımlıysa yalnız o
listedeki komutlar hata tekrar üretiminde seçilebilir. Operatör komutları model
çağırmaz ve güvenli pause ile biter. Görev kimliği, bütçesi ve son tarihi korunur.
`/resume` onaylı model akışını ayrıca sürdürür; API kullanımı doğurabilir.

Her deneme ayrı worker snapshot'ında, seçilmiş değişmez image ve proje ortamıyla
çalışır; host mount/writeback/ağ yoktur. Genel süreç katmanı sürümleri önceden
doğrular. Seçilmiş araç eşlemeleri ve mise tercihleri kontrol içinde de uygulanır.
Aynı invocation ve aynı kaynak/girdi mevcut kaydı döndürür. Kaynak değişmişse
eski invocation reddedilir. Sonucu eksik kalmış intent otomatik yeniden çalışmaz.

`regression_confirmed=true`: aynı görevde gerçekten FAIL olan baseline, aynı
kontrol komutları ve aynı ortamda, farklı kaynak sürümünde PASS olmuştur. Başka
kontroller veya başka ortam karşılaştırması regresyon doğrulaması sayılmaz.

## Sonuçları okumak

| Alan/durum | Anlamı |
| --- | --- |
| PASS | Seçilen kontroller tamamlandı ve geçti |
| FAIL | Gerçek proje kontrolü başarısız |
| FLAKY | Aynı kontrolün tekrarları farklı sonuç verdi veya runner başarısız denemeden sonra retry ile geçti |
| NOT_RUN | Seçilen testler atlandı veya istenen denemeler tamamlanmadı |
| BLOCKED | Gerekli araç/rapor, kaynak veya incelenmiş fiziksel kapasite uygun değil |
| INCONCLUSIVE | Tam veya güvenilir biçimde sınıflandırılabilir sonuç yok |

Farklı iki kontrolün birinin geçip diğerinin kalması FAIL'dir; flaky değildir.
Hata üretmede `operation_status=PASS`, `outcome=FAIL` ve
`failure_reproduced=true` beklenir: işlem hatayı gerçekten üretmiştir. Verify
sırasında PASS dışındaki sonuç, reproduce sırasında üretilemeyen hata, CLI
çıkış kodu **7** verir; JSON kayıt yine yazdırılır. Scriptler bunu hata olarak
iletir. Başlamamış model çağrısı model başarısızlığı olarak gösterilmez.

JUnit test sayıları/skipped/retry işaretleri; Cargo, pytest, unittest ve Node TAP
standart özetleri okunur. Özel runner biçiminde JUnit raporu tanımlayın. Sıfır
test veya okunamayan envanter yalnız exit=0 ile PASS olmaz. Mevcut rapor dosyası
bu çalıştırmada yenilenmemişse eski başarılı rapor kullanılmaz. Çıktı kapasitesi
sessiz kırpılmaz; kapasite engeli açık kalır.

Kısa replay kaydı komut, environment değişkenleri ve ham logları taşımaz. Tam
stdout/stderr/rapor ve native sonuç ayrı şifreli görev artifact'ındadır.
`verification_result_page`, yalnız aynı görevin doğrulama receipt/detail
kayıtlarını tam byte sayfalarıyla okur. Bu ham proje verisi sır içerebilir;
public issue veya PR'a kopyalamadan inceleyin. Intent, başka görev ve gizli QA
kayıtları bu araçla okunamaz. Loglar ajan talimatı veya bağımsız kabul değildir.

## Rust kalite araçları

[Hazırlık rehberi](../scripts/toolchains/verification/README.md) isteğe bağlı
nextest, cargo-llvm-cov, LLVM araçları ve cargo-mutants image'ini üretir. Eksik
araçta başka image'e veya host kuruluma otomatik geçiş yapılmaz.

Onaylı komutlar arasında normal `cargo test --locked --offline` ve ayrıca
`cargo test --doc --locked --offline` korunmalıdır. Nextest doctest'lerin yerine
geçmez. Nextest için örnek `.config/nextest.toml`:

```toml
[profile.melissa]
retries = 0
[profile.melissa.junit]
path = "results.xml"
```

`cargo nextest run --profile melissa --locked --offline` kontrolünün türü
`nextest`, raporu `target/nextest/melissa/results.xml`, biçimi `junit` olur.
Explicit `repetitions` her denemeyi ayrı kaydeder; retry geçmişi temiz PASS'e
indirgenmez. Varsayılan bir tekrardır.

Coverage komutu örneği:
`cargo llvm-cov --json --summary-only --output-path coverage.json --locked --offline`.
Tür `coverage`, rapor `coverage.json`, biçim `llvm_json`. Üretilen satır/branch
sayıları yalnız o çalıştırmanın kapsamıdır; PASS coverage yüzdesinin yeterli
olduğu anlamına gelmez. Projenin eşikleri gerekiyorsa argv'de açıkça tanımlayın.

Hedefli mutasyon örneği:
`cargo mutants --in-place --file src/lib.rs --re add --timeout 30 --output .`.
Tür `mutation`, rapor `mutants.out/outcomes.json`, biçim `mutants_json`.
`--in-place` burada yalnız izole worker kopyasına uygulanır; host kaynak değişmez.
Caught/missed/timeout/unviable ayrı tutulur. Kaçan veya zaman aşan mutant FAIL;
derlenemeyen mutant yakalanmış sayılmaz. Tüm depoyu her düzenlemeden sonra
mutasyona sokmak yerine riskli küçük çekirdek seçilir.

## Gerçek proje debug

Ayarın `debug` alanı isteğe bağlıdır: `language` (`python`, `rust`, `javascript`,
`typescript`), repo köküne göre `entry`, `cwd`, `argv`, TypeScript için `tsconfig`.
Entry ve config görünür kaynak snapshot'ında olmalıdır. Python modülleri/cwd,
Rust'ın alt Cargo projesi ve TypeScript'in extends/sourcemap zinciri kullanılır.
TS çıktı dosyaları worker içindeki özel klasöre yazılır; kaynak breakpoint'i özgün
TypeScript satırına bağlanır. Mevcut debugpy, lldb-dap ve js-debug adapter'ları
kullanılır. İstenen Python/Node/Rust sürümü gerçek debug executable'ıyla
ayrıca karşılaştırılır; eşleşmezse başka sürüm sessizce kullanılmaz. Mise ile
hazırlanmış sürüm için image içindeki açık executable eşlemesini seçin.
Kaynakta `.cargo` veya `rust-toolchain` override'ı bu DAP sözleşmesinde kabul
edilmez; farklı yapı sözleşmesi ayrıca incelenmelidir. Genel CLI'da debug
yalnız açıkça incelenmiş bu ayarla sunulur.

Model `code_debug_start` ve `code_debug_command` kullanabilir. Sahip olunan
oturum kapanmadan kaynak değiştirilmez; iş sonunda `close` gerekir. Rastgele
host PID attach, expression evaluate, host environment veya ağ yetkisi eklenmez.

## Harness geliştiricisi için kontroller

`cargo test --locked -p melissa-cli` çevrimdışı bağlama/regresyon testleridir.
Windows/Docker native testleri varsayılan çalışmada `ignored` kalır; gerekli
image ve güncel CLI yolu seçildikten sonra ayrıca çalıştırılır:

```powershell
$env:MELISSA_PHASE06_IMAGE = 'sha256:REVIEWED_IMAGE_ID'
$env:MELISSA_PHASE06_CLI = 'C:/Build/target/debug/melissa.exe'
cargo test --locked -p melissa-cli --lib project_verification::tests::native_ -- --ignored --test-threads=1
cargo test --locked -p melissa-build-test --test project_debug -- --test-threads=1
cargo test --locked -p melissa-core-contracts --test property_transitions
cargo test --locked -p melissa-repository --test property_contracts
cargo test --locked -p melissa-verification
```

Bunlar gerçek native araç yollarını sınar; canlı AI başarısı iddiası taşımaz.
