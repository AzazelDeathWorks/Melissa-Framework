# Skill, MCP, doküman ve hook bağlantıları

Genel proje görevleri dışarıdan gelen çalışma yöntemlerini, araçları ve doküman
kaynaklarını görev onayına bağlayabilir. Paket içeriği ve ayarlar şifreli görev
kaydında sabitlenir. Sonradan değişen dosya/ayar eski görevin yetkisini değiştirmez.
Araç yanıtı, skill metni veya `allowed-tools` alanı yeni yetki vermez.

## Kullanıcı ve proje skill'leri

Kullanıcı skill dizini `MELISSA_HOME/skills/AD/SKILL.md`, proje skill dizini
`.melissa/skills/AD/SKILL.md` biçimindedir. Yalnız görünür kaynaklar alınır;
ignore/secret/link kuralları resource ve script dosyaları için de geçerlidir.
Öncelik açık import < kullanıcı < proje; aynı adlı proje paketi öne geçer.

```text
/skills
/skill-import C:/MySkills/bug-repro-fix
/extensions C:/MelissaPrivate/extensions.json
```

CLI eşdeğerleri:

```text
melissa --json extension list --workspace C:/Projects/example
melissa --json extension import-skill --path C:/MySkills/bug-repro-fix
melissa --json extension import-skill --path C:/MySkills/bug-repro-fix --digest blake3:...
melissa --json extension configure --workspace C:/Projects/example --path C:/MelissaPrivate/extensions.json
```

Import tüm görünür içeriği hash ile sabitler; eski içerik nesneleri korunur.
Yeni import aynı adın yeni görevlerde kullanılacak pinini değiştirir. Var olan
görev kendi özgün kopyasını kullanır. Model kataloğu ve göreve sözcük eşleşmesiyle
seçilen paketlerin tam talimatlarını görür; diğer paketleri kendisi yükleyebilir.
Bu seçim bir model kalite ölçümü değildir.

`extension_skill` işlemleri: `catalog`, tam `load`, sayfalı `read`, onaylı adlı
`helper`. References, scripts ve assets pinin parçalarıdır. İkili dosyalar hex,
UTF-8 kaynaklar metin ve hex olarak okunur; `next_offset`/`has_more` korunur.
256 KiB'ı aşan araç sonuçları tam şifreli receipt olarak tutulur; modele açık
`extension_result_page` imleci verilir. Sayfalar 32 KiB'a kadar okunur; büyük
doküman/skill içeriği sessizce kesilip tamamlanmış kabul edilmez.
Helper yalnız ayarlardaki açık komutu çalıştırır; model interpreter/argv veya
yetki seçmez. `@skill/scripts/check.py`, sabit paketin worker kopyasına çözülür.
Girdi `.melissa-extension/input.json` dosyasından okunur.

SKILL.md başlığında `name` ve `description` gereklidir. ASCII adlar, sade veya
alıntılı metinler ve çok satırlı açıklamalar desteklenir. Keyfi YAML çalıştırılmaz;
karmaşık YAML nesnelerinden izin çıkarılmaz. Tam özgün SKILL.md korunur. Paket
başına 512 dosya/4 MiB, görev bundle'ı için 8 MiB fiziksel kayıt kapasitesi vardır.
Bu sınırlar model çıktı/token ayarlarını değiştirmez.

On çalışma paketinin başlangıç talimatları [manifestte](../skills/workflows/manifest.json)
bulunur. Bunlar import edilebilir talimat şablonlarıdır. Frontend, servis,
migration, performans veya yayın araçlarının tamamlandığını tek başına kanıtlamaz.

## MCP

[Örnek ayarlar](../examples/cli/extensions.template.json) ayrı bir özel dosyaya
kopyalanıp düzenlenir. Model yalnız bu ayarlarda adı ve `input_schema` değeri
onaylanan araçları çağırabilir. `tools/list` şeması değişirse çağrı kapalı kalır.
[MCP/hook örneğindeki](../examples/cli/extensions.mcp.template.json) `.invalid`
adres ve örnek araç şeması gerçek bağlantıyla değiştirilmelidir; otomatik servis
kurulmaz. Örnek hook yalnız kökteki Python dosyalarının sözdizimini kontrol eder.
Sunucunun açıklaması, annotation'ı veya ek araç ilanı izin kazandırmaz.
`writes:true` araçları salt okunur uzmanlara açılmaz. Uzak sunucunun iç uygulaması
host tarafından doğrulanmaz; uzak erişimin gerçek kapsamını sunucu hesabı belirler.

HTTP ve native worker içindeki stdio taşıması vardır. Açık protokol profilleri:
`2025-06-18`, `2025-11-25`, `2026-07-28`. İlk ikisinde initialize/initialized,
session header ve DELETE kapanışı; modern profilde per-request metadata,
Mcp-Method/Mcp-Name ve onaylı schema'nın Mcp-Param header'ları kullanılır.
JSON ve request SSE yanıtları desteklenir. Otomatik protokol düşürme veya
başarısız tools/call tekrar gönderimi yapılmaz.

Stdio sunucusu host üzerinde başlatılmaz. Projenin seçilmiş image'i ve native
broker izniyle, ağı kapalı özel kaynak kopyasında yaşar. Model turları arasında
aynı oturum kullanılabilir; kaynak değişince durdurulup yeni kopya hazırlanır.
MCP oturumunun stdin/EOF kanalı adapter'a aittir; genel süreç aracıyla ham RPC
gönderilip araç allowlist'i atlanamaz. Büyük mantıksal mesajlar native giriş
çerçevelerine bölünerek aynı protokol mesajı halinde teslim edilir.
Uzman bitince native süreç ağacı kapanır. Host ortamı/credential'ları miras alınmaz.

HTTP bağlantısı yalnız onaylı HTTPS endpoint'e veya açık `loopback:true` yerel
fixture adresine gider. Redirect ve host proxy mirası kapalıdır. `auth_env`
yalnız değişken adıdır; import/katalog/onay sırasında okunmaz. Credential ancak
yetkili çağrıda çözülür, yanıt içinden yansıtılan bearer değeri temizlenir.
Ham anahtar, key içeren URL veya key içeren yapılandırma kullanılmaz.

İstemci tools/list ve tools/call alt kümesini sunar. Sampling, roots, elicitation,
uzak tasks, OAuth tarayıcı akışı, otomatik nested requests, resource/prompt
işlemleri ve eski HTTP+SSE (2024) taşıması bu profilin parçası değildir. Ek giriş
isteği yürütülmez; açık durum olarak döner. Tam MCP uygunluk sertifikası iddiası yoktur.

## Sürüme bağlı dokümanlar

`docs_resolve`, seçili ve bağımlılıkları hazırlanmış worker içinde gerçek kurulu
paket sürümünü ölçer: Python distribution metadata, Node package.json, Cargo
metadata veya Python/Node/Rust runtime sürümü. Kaynakta yalnız bir sürüm aralığı
görmek kurulu sürüm kanıtı sayılmaz. Aynı Cargo paketinin birden fazla sürümü varsa
belirsizlik bildirilir; rastgele biri seçilmez.

Ayarlardaki `versions` haritası tam ölçülen sürümü doküman URL'sine veya Context7
library ID'sine bağlar. Eşleşme yoksa HTTP çağrısı yapılmaz. Sonuç kaynak URL/ID,
sürüm, native kanıt, kaynak/ortam özeti, indirme zamanı ve içerik hash'ini taşır.
Context7 bağlantısı Search Library ve Get Context API'lerini kullanır; key/bakiye
gereksinimi ayrı dış servis koşuludur. Bu özellik geliştirilirken gerçek Context7
servisine ücretli kabul çağrısı yapılmış olduğu iddia edilmez.
[Context7 örneği](../examples/cli/extensions.context7.template.json) belirli bir
paket/sürüm eşlemesidir; kendi kurulu sürümün ve arama sonucunun ID'si incelenir.
UV projesinde sürüm probu `uv run --no-sync` ile o projenin sanal ortamını kullanır;
Python'un izole modu proje dosyalarının probun standart modüllerini gölgelemesini önler.

`docs_search`, yalnız ayarlanmış Context7 veya resmi doküman arama adresine
soru gönderir. `docs_read`, onaylı origin ve yol altındaki sade URL'yi okur.
Yönlendirme, encoded yol kaçışı ve keyfi URL reddedilir. Search/read sonucu tek
başına kurulu sürümle uyum kanıtı değildir; bu ayrım yanıtta belirtilir.

İnsan için model çağrısız helper:

```text
melissa --json extension resolve --task TASK_ID --source python-docs --query "typing"
```

PowerShell karşılığı `scripts/docs-resolve.ps1` içindedir. Bu tek seferlik operatör
helper'ı henüz çalışmamış onaylı proje görevi ister; kayıtları koruyup güvenli pause
durumunda bırakır. Aynı görevin model akışı daha sonra `/resume` ile sürer.

## Hook yaşam döngüsü

Olaylar `before_tool`, `after_tool`, `after_mutation`, `task_end` şeklindedir.
`tools` listesi hook'u belirli wire adlarına daraltır. Komutlar native image ve
özel snapshot içinde çalışır; stdout talimat, dosya yazma izni veya patch değildir.
Üretilen dosyalar host'a kendiliğinden uygulanmaz. Onaylı helper çalışmaları da
aynı kurala bağlıdır.

Ön hook başarısızsa asıl araç çağrılmaz. Son hook başarısızsa asıl işlemin sonucu
ve `mutation_applied` korunur; uygulanmış patch tekrar gönderilmez. Erken kontrolü
başarısız olan gerçek bir yazma da after_mutation olayını üretir. Task end başarı
ve hata yoluna bağlıdır; görev iptali/süre sonu nedeniyle başlamayan hook NOT_RUN
kalır. Hook döngüsü kendi kendini tetiklemez.

Her çağrı/olay için intent ve kaynak/pin/girdi bağlı receipt saklanır. Bilinen
aynı invocation tekrar gönderilmez; yarım intent veya doğrulanamayan uzak etki
INCONCLUSIVE ve uzlaştırma gerektirir. Geçerli native receipt olmadan PASS,
araç PASS sonucuyla da görev kabulü verilmez. Son proje kabul kapıları korunur.

Resmi davranış kaynakları: [Agent Skills](https://agentskills.io/specification),
[MCP taşıma ve sürümleme](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports),
[Context7 API](https://context7.com/docs/api-guide).
