# Her turda bağlam hazırlama

CLI, her gerçek model isteğinden önce çalışma bağlamını yeniden hazırlar.
Görevin özgün kaydı ve araç kanıtları değişmez. Modele gönderilen kısa görünüm,
saklanan tam kayıttan ayrıdır; bir kısa görünüm bağımsız kabul kanıtı değildir.

## Sıcak, yakın ve arşiv bağlamı

- **Hot:** etkin görev ve uzman sözleşmesi, güncel adımın amacı, son araç
  gözlemleri, gerekli araçlar, güncel görev durumu ve bütçe defteri.
- **Warm:** yakın geçmiş, kaynak aralıkları, kabul edilmiş bellek aramalarının
  gözlemleri ve önceki kararlar. Kaynak veya bellek metni yetki vermez.
- **Cold:** eski büyük gözlemler ve tamamlanmış araç alışverişleri. Tam içerik
  şifreli yerel kanıtta tutulur. `history_read` yalnız aynı etkin görev/çalıştırma
  ve aynı kaynak görünürlük politikası altında UTF-8 byte aralıklarını okur.

Varsayılan politika son sekiz mesajı basınç altında arşivlemekten korur;
yakın geçmiş penceresi 64 mesajdır. Bilinen uzun araç çıktıları, en yeni turda
da, hata ve kaynak durumunu koruyan kısmi bir görünüme dönüştürülebilir.
Gözlem görünümü için varsayılan byte hedefi 16 KiB'dir. Bunlar modelin yanıt
veya düşünme token sınırları değildir; bu ayarlar değiştirilmez.

## Kalıcı depo araması

`context_search`, çalışma alanı için şifreli bir FTS5 indeksi tutar. Aynı
çalışma alanındaki yeni görev, kaynak görünürlüğü ve indeks sürümü eşleştiğinde
indeksi yeniden açar. Görünür dosya listesi güncel tutulur. Kullanılabilir bir
Windows değişiklik günlüğü değişmediği doğrulanan dosyaların içeriğini tekrar
okumayı önler. Günlük kullanılamadığında dosya özetleri içerikten hesaplanır.
Değişen dosyalar yeniden indekslenir, kaldırılanlar aramadan çıkarılır ve
sonuç alıntıları güncel dosya içeriğiyle doğrulanır.

Kalıcı kopya çalışma alanı başına 64 MiB ile sınırlıdır. Bu sınırı aşan indeks
oturum içinde çalışır. Bozuk veya eşleşmeyen kopya güncel kaynaklardan yeniden
oluşturulur. İndeks, görev kayıtlarından ayrı ve yeniden üretilebilir bir
önbellektir.

## Boyut hesabı ve zorunlu içerik

Sistem/uzman talimatları, görev/skill/kullanıcı girdisi, güncel hedef, araç
gözlemleri, kaynak aralıkları, bellek, diff/diagnostic, protokol geçmişi ve
şemalar ayrı ölçülür. Bölüm byte tavanları aynı toplam girdi havuzunu paylaşır;
birbirine eklenebilen bağımsız token tahsisleri değildir. SDK'daki
`ContextPolicy.section_byte_limits` ile daha dar açık host tavanı tanımlanabilir.

Son karar, **gerçekte kodlanmış native istek** üzerinden verilir; araç şemaları,
protokol alanları, framing payı ve değişmeyen çıktı rezervi dahildir.
Eşleşen güvenilir `NativeTokenCounter` varsa tam yerel token sayımı kullanılabilir.
Yoksa metin için UTF-8 byte ve framing üst sınırı kullanılır; karakter/4
tahmini kesin token sayısı gibi sunulmaz. Yerel wire tokenizer ölçümü ayrıca
raporlanır, modelin chat şablonu tokenizasyonu veya fatura kanıtı sayılmaz.

Zorunlu içerik sığmıyorsa istek gönderilmeden açık hata alınır. Görseller için
eşleşen native sayaç yoksa önceki davranış korunur ve kapasite kontrolü
`not_measured_multimodal` olarak görünür; doğrulanmış bir token sınırı iddia edilmez.

## Kısa gözlemler ve sıkıştırma

Sıra: eski büyük/tekrarlayan çıktıları referansa dönüştürme, bilinen çıktıları
normalleştirme, uygun tamamlanmış araç turlarını yapılandırılmış kontrol
noktasına taşıma. Her dönüşümde sebep, kaynak özeti, tam kayıt referansı ve
önceki/sonraki byte miktarı bulunur.

Build/test görünümleri çıkış durumunu, geçen/başarısız/atlanan sonuçları,
hata konumlarını, kaynak sürümünü ve eksik çıktı bilgisini korur. Tekrar
koşuları ayrı sayılır. Arama sonuçlarında kaynak özeti, satır aralığı ve
varsa tam byte aralığıyla sınırlı UTF-8 alıntı yer alır. İkili dosyaya metin
alıntısı uydurulmaz. Diff görünümü dosya başlıklarını ve sınırlı hunk
alıntılarını içerir; Git'in tırnaklı yol gösterimi çözülmüş dosya yolu sayılmaz.
`full_log_ref` / `diff_ref` özgün gözlemi geri okumaya yarar.

Kullanıcı talimatları, asistanın metinsel kararları ve başarısız araç turları
kontrol noktasından çıkarılmaz. Yalnız eski, tamamlanmış, araç çıktısı başarılı
olan uygun alışverişler gruplanır; çağrı ve sonuçları birlikte taşınır.
Özgün native geçmiş **dönüşümden önce** doğrulanır. İmzalı/şifreli düşünme ve
tanınmayan devam alanları aynen korunur; başka sağlayıcı/model kökenine
taşınmaz. Belirsiz geçmiş sessizce düzeltilmez.

## İsteğe bağlı özetler ve destek sınırı

Derleyici kendiliğinden ek model çağrısı yapmaz. SDK'daki `ReviewedSummary`
yolu yalnız hostun seçtiği, tam kaynak bytes'ı ve özet özetiyle eşleşen ayrı
inceleme kaydı bulunan dış özeti kabul eder. Bu özet de güvenilmeyen veri
olarak kalır ve özgün kaydın yerini almaz.

Mevcut sağlayıcı kayıtlarında doğrulanmış native compaction adaptörü yoktur.
Bu aşama `not_registered` kalır; uyumlu bir protokol adı görülmesi destek
varsaymak veya ek ücretli çağrı yapmak için kullanılmaz. Bu belge otomatik
model özeti veya native compaction'ın canlı doğrulandığını iddia etmez.

## Tekilleştirme ve yeniden açma

Uzman talimatı native sistem kanalında, araç şemaları native araç kanalında
bir kez iletilir. Başlangıç paketi bunlara özetle bağlı referanslar taşır.
Tam çıktı şeması typed candidate aracıyla aynıysa tek kopya o araçta
gönderilir. JSON-object modunda gerekli metinsel şema korunur. Çelişen aynı
isimli şemalar reddedilir; yetki veya güvenlik sözleşmesi atılmaz.

CLI, adım/uzman için zaten yetkilendirilmiş araçları kullanır. Bu özellik
otonom model seçimi, yeni araç yetkisi veya paralel ajan yürütmesi eklemez.

Derleyici politikası, kaynak görünürlüğü ve etkin adım devam kaydına bağlanır.
Eski runtime politikasıyla mühürlenmiş bir çalıştırma değişmiş politikayla
sessizce yeniden açılmaz; özgün kayıt korunur. Aynı politika altında yeniden
açılan çalışma, tam geçmişten aynı kısa görünümü tekrar oluşturabilir.

Her istek kanıtındaki `context_report.intelligence`, bölüm ölçümleri,
arşiv gerekçeleri, kontrol noktaları ve hazırlama süresini içerir. Yerel
protokol testleri doğruluk ve boyut davranışını ölçer; canlı model başarısı,
ücret tasarrufu veya üretim gecikmesi sonucu değildir.
