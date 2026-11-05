# Proje komutları ve ortamı

Normal proje görevleri özel bir kabul recipe'si gerektirmez. Terminal, proje
manifestlerini inceler ve seçilen ortamla yürütme sözleşmesini hazırlar.
Model/görev kalite kabulü, komutun çalışmış olması ve yayın kabulü ayrı şeylerdir.

## Başlamak

Proje klasöründe `melissa` aç:

```text
/config C:/MelissaPrivate/settings.json
/budget 5
/duration 60
/project-doctor
src/main.py içindeki hatayı düzelt ve testleri çalıştır
/plan
/create
/details
/approve
/run
```

`/budget`, yeni görevin toplam API harcama tercihidir; modelin düşünme veya
çıktı kapasitesini azaltmaz. `/duration` dakika cinsinden toplam görev/yeni
süreç süresini seçer. Var olan onaylı görevin bütçesi ve bitiş zamanı değişmez.
Model ayarları veya harcama tercihi yoksa `/create` yalnız hazırlık kaydeder.
Plan kipinde de hazırlık kaydedilir. Eski hazırlığı yürütme iznine çevirmek
yerine `/new AMAÇ` ile incelenebilir yeni bir yürütme sözleşmesi oluşturulur;
eski kayıtlar korunur. Bu adım yeni izin verdiği için önceki plan onayı yürütme
onayı yerine kullanılmaz.

Kabul komutu keşfedilemezse açık bir kontrol eklenebilir:

```text
/check {"executable":"python","argv":["-m","unittest","discover"],"cwd":"."}
```

Kontroller beklenen gizli cevapları tahmin etmez. Modelin “test geçti” demesi
yeterli değildir; son kaynak üzerinde komutları Rust yürütücüsü ayrıca çalıştırır.

## Ortam seçmek

Varsayılan ortam mevcut değişmez SDK image'idir. `/environment` seçimi gösterir;
`/environment sha256:...` başka hazırlanmış bir image'i yalnız bu proje için
seçer. Ayrıntılı ayar için [örnek JSON](../examples/cli/project-environment.template.json)
kopyalanıp `/environment C:/MelissaPrivate/project-environment.json` ile yüklenir.
Tercih proje kök kimliğine bağlı şifreli olarak saklanır; global Node, Python,
Rust veya paket yöneticisi varsayılanı değiştirilmez. Yeni görev bu tercihin
tam özetini onaya bağlar; sonraki yapılandırma eski görevi sessizce değiştirmez.

`versions` örneği `{"node":"22.14.0","python":"3.13.5","rust":"1.98.1"}`;
seçiciler sayısal sürümlerdir. `executables`, image içindeki açık araç yollarını
seçebilir: `{"node":"/opt/project-node22/bin/node"}`. Windows host exe'si,
host PATH'i, kullanıcı profili veya credential aktarılmaz.

`use_mise:true`, hazırlanmış image'deki mise üzerinden açık sürümleri seçer.
Mise otomatik kurulumları ve çalışma anında ağ erişimi kapalıdır. Rust seçimi
süreçte `RUSTUP_TOOLCHAIN` ile, uv Python seçimi `UV_PYTHON` ile uygulanır;
iki projenin seçimi birbirini değiştirmez. Çalışma anında eksik bir sürüm veya
araç sessizce başka sürümle değiştirilmez: `BLOCKED` kaydı oluşur.

## Proje keşfi ve bağımlılıklar

Kök ve görünür alt projelerde Cargo.toml/lock, package.json ve Node kilitleri,
pyproject.toml/uv.lock/requirements.txt okunur. `.node-version`, `.nvmrc`,
`.python-version`, rust-toolchain.toml, mise.toml ve .tool-versions istekleri
kaydedilir. Çelişkili/karmaşık seçiciler açık ortam seçimi gerektirir. Discovery
herhangi bir hook, test, build veya kurulum çalıştırmaz.

- Cargo: kilitli, çevrimdışı check/build/test ve seçilmiş rustup toolchain.
- npm: `ci --offline`; pnpm: `--offline --frozen-lockfile`.
- Yarn Classic: `--offline --frozen-lockfile`; yeni Yarn: immutable lock/cache.
- uv: `sync --frozen --offline --no-python-downloads`, ardından `run --no-sync`.
- pip: `--no-index --require-hashes` ile onaylanan requirements dosyası.

Pip bağımlılıkları proje altındaki özel sanal ortama kurulur; image'in Python
kurulumu değiştirilmez. Hazırlanmış image'de `/opt/melissa-project-cache/`
altındaki `cargo/registry`, `cargo/git`, `uv`, `npm`, `pnpm`, `yarn` önbellekleri
özel diske alınabilir; `wheels` dizini pip'in çevrimdışı wheel kaynağıdır.
Host önbelleği veya kimlik bilgisi taşınmaz. Cargo ayrıca image'deki
`/usr/local/cargo/registry` ve `git` önbelleklerini kullanabilir.

Bağımlılık ve gerekli araçlar seçilen hazırlanmış image/cache'de bulunmalıdır.
Bu sürüm keyfi dış ağa veya host üzerinde paket kurulumuna izin vermez. Eksik
bağımlılık PASS sayılmaz. Sync ve asıl komut aynı özel çalışma kopyasını kullanır;
başka bir oturuma global kurulum sızmaz. Kalıcı image hazırlığı ayrı bir operatör
işidir; `/environment-sync` host'a kurulum yapıldığı anlamına gelmez.

## Süreç oturumları

Modeldeki `project_process` aracı başlatma, onaylı kontrol, ön kontrol, sync,
poll, wait, stdin, EOF ve cancel işlemlerini sunar. Yazıcı uzman genel argv
çalıştırabilir. Salt okunur test uzmanı yalnız sözleşmedeki adlandırılmış
kontrol/ortam işlemlerini başlatır. Hiçbir model image, kaynak limiti, ağ veya
yetki seçemez. Host her gerçek çağrı için tam komut ve snapshot'a bağlı ayrı
broker izni üretir. Başlatma yanıtı komutun bitişi veya test başarısı değildir.

İnsan için onaylı görevin `/exec` komutu da executable/argv/cwd JSON nesnesi
alır. `/preflight` ve `/environment-sync` model çağırmadan çalışır. Bu operatör
işlemleri, durmuş süreç kanıtıyla görevi güvenli pause noktasında bırakır;
model akışı daha sonra `/resume` ile aynı görevden başlar.

Başka terminalden süreç çıktısı ve kontrolü:

```text
melissa --json project poll TASK_ID SESSION_ID --after 0
melissa --json project stdin TASK_ID SESSION_ID --hex 68656c6c6f0a
melissa --json project eof TASK_ID SESSION_ID
melissa --json project cancel TASK_ID SESSION_ID
```

Stdout ve stderr ayrı ham byte akışlarıdır; JSON'da hex ile kayıpsız taşınır.
`next_cursor`/`has_more` ile bütün sayfalar okunabilir; önceki çıktı silinmez.
Stdin istek kimliği tekrarlandığında aynı veri ikinci kez kuyruğa konmaz.
`input_delivered_to_pipe`, verinin çocuk sürecin pipe'ına yazıldığını kanıtlar;
uygulamanın onu işlemiş olduğunu iddia etmez. Çıktı sayfa boyutu bir hedeftir;
tek bir olay bölünmez ve gerekirse ilk olay bu hedefi aşabilir.
İptal yalnız göreve ait doğrulanmış oturumu hedefler; ham host PID'si alınmaz.
İstek kaydı duruş kanıtı değildir; yerel süreç ağacı kapanışı ayrıca doğrulanır.

Aktif süreç modeli turları ve ayrı kontrol bağlantıları arasında yaşar. Kayıtlar
yeniden açılır; sahibi ölmüş veya süresi bitmiş aktif kayıt `INCONCLUSIVE` ve
uzlaştırma gerektirir. Eski süreç körlemesine yeniden başlatılmaz. Bu sürüm,
çöken host sürecinin canlı stdin bağlantısının otomatik devralındığını iddia etmez.

## Kaynaklar ve dosya kapsamı

Komutlar host mount'u olmayan, ağı kapalı, salt okunur image kökü ve kapasiteli
özel tmpfs üzerinde çalışır. Host kaynakları kopyadır; üretilen dosyalar host'a
otomatik uygulanmaz. Kalıcı kaynak değişiklikleri mevcut kapsamlı dosya/patch
araçlarıyla yapılır. Komut süreçlerinde workspace arşivi alınmaz (`export_bytes:0`).

Geliştirme görevinde `/scope DOSYA` farkını güvenli pause sonrasında kabul edip
`/resume` ile aynı göreve ek dosya yetkisi verilebilir. Bu özellik ilk proje
yürütme onayında belirtilir. Özel eski oracle tarifleri genişlemez. Korunan
dosyalar değişmez; görev kimliği, kabul komutları, bütçe ve bitiş zamanı korunur.
`/service` bu sürümde yalnız gereksinim kaydıdır; servis başlatma izni değildir.

RAM, CPU oranı, toplam CPU zamanı, disk/tmpfs, PID ve çıktı kapasitesi ortamda
görünürdür. `/project-doctor` yapılandırmayı ve aktarım sınırlarını gösterir;
native preflight gerçek tool sürümleri, cgroup CPU/RAM/PID ve boş disk bilgisi
üretir. Yapılandırılmış RAM, fiziksel boş RAM diye sunulmaz. CPU oranı hard,
toplam CPU tüketimi izlemeli sınırdır. Yarım/bozuk çıktı PASS olamaz.

Mevcut aktarım zarfı 1024 dosya/64 MiB, kaynak deposu varsayılanı 10.000 dosya/
128 MiB'dir; doktor ve dispatch ön kontrolü bunları görünür biçimde denetler.
Desteklenmeyen kapasite sessiz kırpılmaz. Bunlar model token/araç limiti değildir.
Windows'ta veri alanı için kısa bir `--home` yolu önerilir. Yerel doğrulamada
çok uzun özel kanıt kökünde genel depo hatası gözlendi; kısa kökte aynı işlem
geçti. Bu gözlem, bütün uzun yol koşullarının desteklendiği anlamına gelmez.

## Sonuçların anlamı

Native komutun gerçek sıfır çıkışı, bozulmamış çıktı ve doğrulanmış süreç duruşu
PASS gerektirir. Ortam engeli `BLOCKED`, başlanmamış iş `NOT_RUN`, belirsiz
etki `INCONCLUSIVE` kalır. Son proje kabulü onaylı komutları son kaynak/ortamda
bağımsız host yürütmesiyle bağlar. Bu açık proje testidir; gizli oracle, bütün
özellik kapsamı veya üretim/yayın kabulü olduğu iddia edilmez. Teslim kaydı
kaynak manifesti ve komut kanıtlarını taşır; kurulum paketi değildir.

Komut davranışlarının resmi kaynakları: [mise exec](https://mise.jdx.dev/cli/exec.html),
[uv lock/sync](https://docs.astral.sh/uv/concepts/projects/sync/),
[rustup override](https://rust-lang.github.io/rustup/overrides.html),
[npm ci](https://docs.npmjs.com/cli/v11/commands/npm-ci/),
[Corepack packageManager](https://github.com/nodejs/corepack).
