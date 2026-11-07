# Büyük depo ve güvenli yapısal değişiklik

Normal kaynak erişimi `.gitignore`, `.ignore`, açık dışlamalar ve hassas dosya
politikasını dosya gövdelerini okumadan uygular. Alt klasör kuralları ve izin
verilen yeniden dahil etme kuralları desteklenir. Dışlanan bir üst klasörün
içinden dosya yeniden dahil edilmez. `target`, `node_modules`, `.venv` ve
`__pycache__` her derinlikte kaynak kapsamının dışındadır. Global Git ayarları
okunmaz. Tam Git/paket yakalama kendi ayrı host sözleşmesini korur.

Kaynak varsayılanı 50.000 görünür dosya/dizin girdisi, tek dosyada 8 MiB ve
toplam 256 MiB'dır. Worker aktarımının fiziksel üst sınırı 100.000 dosya ve
512 MiB'dır; görevde seçilen disk kapasitesi daha küçükse o uygulanır. Aşımda
dosyalar sessizce atılmaz: işlem açık kapasite hatasıyla durur. Bunlar modelin
düşünme, cevap veya araç çağrısı bütçesini azaltmaz. Eski onaylanmış görevlerin
kayıtlı sınırları değiştirilmez. USTAR aktarım formatına sığmayan yollar açık hata verir.
Kaynak/ignore kapsamı değişen eski görev için yeni kaynak incelemesi gerekir.

## İndeks, materyalizasyon ve güncellik

Mevcut şifreli artımlı proje indeksi korunur. Kaynak gövdeleri ayrıca aynı
depo/host süreci içinde yeniden kullanılır. NTFS dosya kimliği ve geçerli USN
journal kanıtı olmadan yalnız zaman damgasına güvenilmez; kanıt bulunmuyorsa
dosya yeniden okunur. Değişiklik, silme, dosya kimliği veya aktif ignore kuralı
değişimi yeniden gözlenir. Patch uygulaması ayrıca güncel preimage doğrular.

`materialization` kaydı `source_bytes_read`, `source_bytes_reused`,
`transfer_bytes` ve `files` alanlarını taşır. Okuma sayacı materyalizasyon
adımını ölçer; daha önce yapılan ilk snapshot/indeks okumasını içermez.
`transfer_bytes` kontrol dosyaları hariç kaynak yüküdür; tek başına başarılı
süreç başlatma kanıtı değildir. Bağımsız worker tam snapshot alır; aynı
stdio oturumunda poll/stdin/EOF için snapshot yeniden aktarılmaz. Bağımsız
worker'lar arasında delta aktarımı veya kalıcı konteyner cache'i uygulanmaz.

## Model araçları

| Araç | Davranış |
| --- | --- |
| `code_semantic` | Gerçek rust-analyzer/TypeScript/Pyright üzerinden definition, references, type_definition, implementation, symbols, diagnostics ve hover. Desteklenmeyen sunucu yöntemi hata olarak kalır. |
| `code_impact_map` | Değişen dosyalardan ters import geçişi, en yakın paket manifesti ve aynı paket test adayları. Çözülemeyen alias/importlar, sözdizimi hataları ve desteklenmeyen dosyalar ayrı döner. |
| `code_structural_search` | Rust/Python/JavaScript/TypeScript/TSX içinde ast-grep 0.51.1 yapısal eşleşmeleri; seçili dosyaların tam sürümleri. |
| `code_structural_propose` | `pattern` ve `rewrite` ile tam preimage bağlı çok dosyalı öneri. Kendi başına yazmaz. |
| `fs_patch` | Mevcut görev/profil/dosya izni, kesin kaynak sürümü ve byte doğrulamasıyla öneriyi uygular. |
| `fs_artifact_page` | Büyük araç sonucunu özet işaretçisinden son baytına kadar okur. Kaynak hash'i olan kanıtta CURRENT/STALE, diğerinde UNKNOWN güncelliği döner. |

TypeScript'te yakalanan tsconfig, extends, project references ve paths alias
ayarları gerçek sunucuya sunulur. Python'da var olan pyrightconfig korunur.
SDK seçimi ve sunucu komutu kaynak tarafından değiştirilemez; Rust build
script/proc-macro çalıştırması varsayılan kapalıdır. Sonuç konumları yakalanan
kaynak veya sabit SDK dosyalarına bağlanır.

Etki haritası bir build sistemi garantisi değildir. Dinamik importlar,
makrolar ve çözülemeyen isimler eksik ilişki bırakabilir. AST eşleşmesi de
semantik rename değildir: gerçek çağıranları LSP ile doğrula, gereken dosyaları
incele ve sonuçta tip/build/test kontrolünü çalıştır. Belirsizliği gizleme.

## Operatör yardımcıları

Başlatılmış Melissa home ile, model veya API anahtarı gerekmeden:

```powershell
melissa --json repository impact-map --workspace C:\project --query C:\private\impact.json
melissa --json repository structural-patch --workspace C:\project --query C:\private\structural.json
```

`impact.json`:

```json
{"paths":["src/calc.ts"]}
```

`structural.json`:

```json
{"language":"typescript","paths":["src/first.ts","src/second.ts"],"pattern":"api.legacy($A)","rewrite":"api.current($A)"}
```

`rewrite` verilmezse yalnız arama yapılır. `$A` tek düğüm, `$$$ARGS` çoklu
düğüm yakalar. Bağlanmamış değişken, hatalı kaynak/pattern, çakışan eşleşmeler
ve kota aşımı açıkça reddedilir. Yorum/string içindeki benzer metni otomatik
olarak çağrı sayma. Yardımcılar `scripts/impact-map.ps1` ve
`scripts/structural-patch.ps1` aynı salt okunur CLI yolunu çağırır. Uygulama
için onaylı görevin `fs_patch` yolunu kullan; JSON önerisi yetki değildir.

## İstem ve sağlayıcı cache'i

Araç şemaları her istekte ada göre sıralanır; uzman sistem talimatı konuşmadan
ayrıdır. Varsayılan `prompt_cache` kapalıdır. Sağlayıcının desteklediği kesin
doğrulanmışsa yapılandırmada `openai_memory` veya `anthropic_ephemeral` açıkça
seçilebilir; yanlış protokol eşleşmesi reddedilir. Eski ayarların özeti değişmez.
Özel uyumlu gateway'lerde destek varsayılmaz; hata halinde başka protokole
sessiz geçiş yapılmaz. Cache anahtarı statik prefix'in hash'idir, ham talimat
veya kimlik bilgisi içermez. Model/çıktı kapasitesi, tam konuşma ve araç
sonuçları korunur. Cache isabeti ve ücret tasarrufu yalnız sağlayıcının gerçek
usage kaydıyla söylenebilir; yerel JSON testi bunu kanıtlamaz.

Protokol kaynakları: [ast-grep Rust API](https://docs.rs/ast-grep-core/0.51.1/ast_grep_core/),
[LSP](https://microsoft.github.io/language-server-protocol/),
[Responses prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching),
[Messages prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching).
