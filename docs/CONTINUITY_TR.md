# Bellek, uzun görev ve güvenli geri alma

Bu özellikler mevcut yerel, şifreli görev kaydını kullanır. Hindsight kurulmaz.
Aşağıdaki inceleme, checkpoint, kaynak uzlaştırma ve geri alma komutları model
veya sağlayıcı çağırmaz. `task resume` ise normal model yürütmesini başlatır.
Yerel doğrulama, canlı model kalitesi veya temiz Windows kabulü değildir.

## Açıklanabilir proje belleği

`memory_propose` mevcut `key`, `claim` ve `source_ids` alanlarına ek olarak
`kind` ve isteğe bağlı `expires_after_ms` alır. Saat, görev, branch ve commit
kimliklerini uygulama gözlemler; model bu alanları belirleyemez.

| Tür | Geri çağırma koşulu |
| --- | --- |
| `project_decision` | Operatör kabulü, güncel kaynak ve aynı branch bağlamı. |
| `user_preference` | Operatörün açıkça incelediği proje tercihi; kod değişikliğinden ve branch geçişinden bağımsız. Kaynak görünürlüğü ve varsa süre sonu yine uygulanır. |
| `transient_state` | Kaynak/branch koşullarına ek olarak yalnız doğduğu görev; `expires_after_ms` zorunlu. |
| `failure_experience` | Operatör kabulü, güncel kaynak ve aynı branch bağlamı. Eski deneyim güncel çalıştırılmış kanıt yerine geçmez. |
| `unclassified` | Tür seçilmeden yapılan yeni öneri; kaynak ve branch denetimi sürer. Eski kayıtlarda bulunmayan bilgiler `unknown` kalır. |

Örnek model araç girdisi:

```json
{"operation":"memory_propose","key":"formatter","claim":"Use the repository formatter before review","kind":"project_decision","source_ids":["package.json"]}
```

Öneri kendiliğinden kabul edilmez. `memory list --workspace PATH` kaynağı,
tarihi, türü, branch'i, süreyi, tutulma nedenini, çakışan anahtarları ve bütün
kitabın `review_digest` değerini gösterir. `memory accept --id ID --workspace PATH
--digest DIGEST` yalnız güncel incelemeyi kabul eder; aynı anahtardaki önceki
kabul edilmiş kayıtlar açık incelemenin parçası olarak superseded olur.
Kaynak değişmiş karar/deneyim eski sayılır; güncel kaynakla yeni öneri gerekir.
`forget` geri çağırmayı durdurur, şifreli tarihsel denetim kaydı korunur.

Git gözlemi yalnız projenin kendi sabitlenmiş `.git` dizinini okur; Git komutu,
hook veya yapılandırma çalıştırmaz. Normal branch, packed ref, detached HEAD ve
henüz commit'i olmayan branch tanınır. Başka yere yönlendiren `.git` dosyası
(bağlı worktree dahil) veya okunamayan metadata `unavailable` olur. Böyle bir
bağlamda branch'e bağlı yeni bilgi geri çağrılmaz; branch uydurulmaz. Git'siz
projeler kendi proje kimlikleri içindeki `not_git` bağlamını kullanır.

## Uzman seçimi ve yaklaşım geçmişi

Hazırlık çıktısındaki `adaptive_plan.specialist_selection`, risk, iş boyutu ve depo
sinyallerinin hangi uzman sırasını seçtiğini açıklar. Bu seçim mevcut yerel
kurallara dayanır; ölçülmüş model başarı puanı iddiası değildir.

Kaynak sürümüne bağlı handoff, başarısız araç adı + tam girdi özeti + kaynak
özeti + kanıt referansını taşır. Aynı komutun aynı kaynakta tekrar başarısızlığı,
değiştirilmiş komuttan ayrılır. Başarılı aynı yaklaşım çözüldü olarak kaydedilir.
Mevcut yeniden planlama denetimi bu geçmişi kullanır; yeni bir araç çağrısı veya
çıktı-token tavanı eklenmez. Model araçları geçmişi değiştiremez.

## Checkpoint, kalan iş ve devam

```text
melissa --json task continuity TASK
melissa --json task checkpoints TASK
melissa --json task checkpoint TASK --label "before refactor"
melissa --json task progress TASK
```

Görünüm özgün görev durumu, kaynak özeti, checkpoint'in halen güncel olup
olmadığı, uzman kilometre taşları, kalan işler, harcama ve belirsiz işlemleri
birlikte gösterir. Elle alınan checkpoint bir inceleme kaydıdır; kaynak
byte'larını geri yüklemez ve ek yürütme yetkisi vermez.

Güvenli şekilde duraklatılmış görev aynı kaynak/sürüm/ayar ve özgün hesapla
`task resume TASK` üzerinden devam eder. Bilinmeyen sağlayıcı veya native araç
etkisi otomatik tekrar gönderilmez. Kaynak kullanıcı tarafından değişmişse:

```text
melissa --json task adopt-preview TASK
melissa --json task adopt-source TASK --digest REVIEW_DIGEST
melissa task resume TASK
```

Operatör önce değişen yolları ve güncel plan/bütçeyi inceler. Aynı inceleme
özetiyle kaynak benimsendiğinde eski uzman çıktıları ve lease'ler geçersiz olur;
uzmanlar yeni nesil/koşu kimliğiyle yeniden başlar. Eski kanıtlar, ilk kabul
sözleşmesi, yazma kapsamı, harcama ve bitiş zamanı korunur. Bekleyen eski plan
önerisi uygulanmaz. Korunan kabul dosyası değişikliği, etkin veya belirsiz işlem
ve değişmiş uygulama/ayar/süre bu yolu durdurur. Kaynak benimseme dosya yazmaz.

`task replan` ve oturumun mevcut plan inceleme yolu kalan uzman adımlarını
aynı görev yetkisi içinde değiştirir; geçmiş veya bütçe sıfırlanmaz.

## Tur veya seçili parçaları geri alma

```text
melissa --json task changes TASK
melissa --json task undo-preview TASK --request-json '{"turn_request":"REQUEST_ID"}'
melissa --json task undo TASK --request-json '{"turn_request":"REQUEST_ID"}' --digest REVIEW_DIGEST
```

Tek makbuzun seçili özgün native edit indeksleri için seçim:

```json
{"receipts":[{"receipt_id":"INVOCATION:receipt","hunks":{"src/main.rs":[0,2]}}]}
```

`task changes` dosyaları, edit indekslerini, özgün öneriyi, tur/koşu bağını ve
native makbuzları gösterir. `hunks` boş bırakılırsa makbuzun tamamı seçilir.
Bir turdaki birden fazla dosya işlemi kaydedilen sıranın tersinde hesaplanır.
Seçili parçaların ardışık geri alınması desteklenir. Önizleme bütün seçimi
bellekte doğrular; etkiler başlamadan güncel kaynak ve inceleme özeti yeniden
kontrol edilir.

Geri alma yalnız aynı görevin başarıyla sonuçlandığı kanıtlanmış native
patch/create/delete/move işlemleri içindir. Tam son içerik, dosya kimliği ve
hedef yolun boşluğu kontrol edilir. Araya giren kullanıcı düzenlemesi aynı
dosyada ayrı bir satırda olsa bile çatışma olur; otomatik birleştirme veya
üzerine yazma yapılmaz. Dosyası değişmiş turdan daha eski turu geri almak için
önce sonraki değişikliği ayrı incelemek gerekir.

Tekil boş dizin create/delete/move ters işlemi de native broker kullanır.
Dizin dolmuşsa işlem reddedilir. Birden fazla dizin veya dizin/dosya karma
seçimi etkiler başlamadan reddedilir; recursive geri yükleme yapılmaz.
API/DB değişikliği, süreç etkisi ve Git geçmişi geri alınmaz.

Operatörün incelediği ters işlemler aynı şifreli kayıtta, model maliyeti sıfır
olan ayrı bir yerel işlem kimliğiyle yürütülür. Özgün görevin durumu, bütçesi ve
kanıtları korunur. `PASS` yalnız ters işlemin dosya/dizin sonucunun doğrulandığını
söyler; uygulama veya özgün görev kabulü değildir. Birden fazla işlem atomik
değildir. Kısmi/belirsiz sonuç `INCONCLUSIVE` olur ve görev devamı uzlaştırma
bekler. Yarım kalmış intent otomatik tekrarlanmaz. Aynı onaylı işlemin tekrar
sorgusu tarihsel makbuzu döndürür; dosyaları yeniden değiştirmez.
CLI geri alma sonucu PASS değilse çıkış kodu 7'dir.

Terminal oturumunda aynı inceleme yolları: `/continuity`, `/checkpoints`,
`/checkpoint LABEL`, `/changes`, `/undo-preview JSON`,
`/undo {"selection":{...},"digest":"REVIEW_DIGEST"}`, `/adopt-preview` ve
`/adopt-source REVIEW_DIGEST`. Bu komutlar etkin seçili görevi kullanır.
