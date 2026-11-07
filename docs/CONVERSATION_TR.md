# Konuşmalı proje oturumu

Proje klasöründe `melissa` açıp amacını yaz. Hazırlık yerel kaynak incelemesi
ve açık niyet kuralları kullanır; bir model tarafından üretilmiş plan olarak
sunulmaz. `/plan` önerilen dosyaları ve uzman adımlarını, `/details` tam kaydı
gösterir. `/create` özel kabul tarifi veya model anahtarı olmadan proje hazırlık
görevi kaydeder. `/approve` bu hazırlık kaydını kabul eder; model veya süreç
izni oluşturmaz.

Genel yürütme için model ayarı, `/budget` ve incelenmiş proje ortamı gereklidir;
[proje çalışma rehberi](PROJECT_RUNTIME_TR.md) adımları açıklar. Bunlar olmadan
oluşturulmuş hazırlık kaydı `/run` ile API çağrısı yapmaz. Mevcut
özel kabul tarifiyle yürütme yolu ayrıca çalışır; projeden ayrı `/acceptance`
dosyası bu yolun bağımsız kabul sözleşmesidir. Genel kullanım için bu dosyayı
elle hazırlamak zorunda değilsin.

## Kipler ve görev devamlılığı

- `/mode plan`: değişikliği planla; yazmalı yürütme başlatılmaz.
- `/mode review`: inceleme hazırla; yazma kapsamı genişletilmez. Yazmalı eski
  bir yürütme tarifi bu kipte çalıştırılmaz.
- `/mode develop`: geliştirme kapsamı hazırla. Kip tek başına araç, dosya,
  model, bütçe veya servis yetkisi vermez.
- Görev seçiliyken normal mesaj veya `/steer MESAJ`, aynı göreve yönlendirme
  ekler. Görev kimliği, eski mesajlar ve model harcama kaydı sıfırlanmaz.
- Ayrı bir işe geçmek için `/new AMAÇ` kullan. `/open GÖREV_KİMLİĞİ` aynı
  projedeki önceki konuşmayı açar. Etkin iş güvenli durmadan kip değiştirilmez.

## Proje kuralları

Genel kullanıcı talimatı, etkin Melissa veri klasörünün `AGENTS.md` dosyasıdır
(`--home` ile seçilen klasör). Proje kökündeki ve görünür alt klasörlerdeki
`AGENTS.md` dosyaları ayrıca yüklenir. `/instructions` güncel içerikleri,
uygulanacak dizinleri ve özetlerini gösterir.

Bir dosya için öncelik kullanıcı → proje kökü → en yakın üst klasördür.
Bir klasörün talimatı kardeş klasöre uygulanmaz. Açık kullanıcı isteği ve
yürütücünün izin kuralları üstündür. Bu belgeler yeni yetki, bütçe veya görev
kabulü oluşturamaz. Model bunları değiştiremez; kullanıcı değiştirirse sonraki
model turunda yeniden okunur. Gizli ve dışlanan klasörler taranmaz; okunamayan
veya bozulmuş talimat sessizce atlanmaz.

## Ek dosya ve servis gereksinimi

`/scope src/new_file.rs` veya `/service postgres` ek kapsam farkı hazırlar.
Görünen dosya/servis listesini inceleyip `/scope-approve GÖSTERİLEN_ÖZET` ile
hazırlık kapsamını kabul edebilirsin. Birden çok ek istek birikir; yeni fark
önceki özeti geçersiz kılar. Zaten kayıtlı kapsam yeniden onay istemez.
Korunan veya proje dışındaki dosyalar kabul edilmez.

Bu onay hazırlık kaydını günceller. Yeni proje yürütmeleri ilk onayda bu
özelliği seçmişse ek dosya kapsamı güvenli pause noktasında kabul edilip
`/resume` ile etkinleşir. Özel eski oracle tarifleri genişlemez. Görev kimliği,
uzman yetki tavanı, kabul komutları, süre ve bütçe değişmez. Servis başlatıldığı
veya yeni dosyada henüz yazma yapıldığı varsayılmaz.

## Çalışan görevi yönlendirme

Yetkili mevcut yürütme sırasında terminal canlı aşama, olay ve kayıtlı maliyet
değişimlerini gösterir. Aynı satıra mesaj, `/pause`, `/cancel`, `/status` veya
`/answer SORU_KİMLİĞİ YANIT` yazılabilir. Çalışırken Ctrl+C iptal isteğidir;
çalışan gelecek veya worker bırakılarak terminalden çıkılmaz. `/exit` önce
güvenli duraklatma ister; duruş sonucu geldikten sonra çıkılır.

`ask_user` aracı gerçekten gerekli tek bir soruyu kaydeder ve açık yanıtı
bekler. `/questions` bekleyen soruları gösterir. Bekleme sırasında yeni model
isteği yapılmaz. Yanıt, pause veya cancel bekleyişi çözer; eksik cevap
uydurulmaz. Mesajlar ve kurallar yalnız yerleşmiş araç gruplarının arasında
modele aktarılır; başlamış bir etkinin anında geri alındığı iddia edilmez.

Etkileşimli giriş gerçek terminal içindir. Yönlendirilmiş standart girişte
`/run` bitene kadar aynı girdi akışı okunmaz; başka oturumdan aynı görevi
açıp mesaj/kontrol göndermek mümkündür. Ham model token akışı yerine görev
ilerlemesi gösterilir; tam kanıt ayrı görev kayıtlarında kalır.

## Kayıt ve kabul

Konuşma revizyonları Windows kullanıcı anahtarıyla şifrelenir ve göreve bağlı
doğrulanır. Her mesajda bütün plan kopyalanmaz; ekler ve değişiklikler saklanır.
Okuma ve yayımlama kilitleri eşzamanlı başlık değişimini korur. Yarım kalmış
revizyon ezilmez veya sessizce tekrar oynatılmaz. Host kayıt zarfına sığmayan
bir kayıt önceki geçerli başlık değiştirilmeden reddedilir.

Gereksinim → uzman alt işleri → `/diff` → gerçek test kayıtları → bağımsız
teslim/kabul bağlantısı görünürdür. Konuşma kaydı test kanıtı değildir.
Yeni hazırlıkta testler `NOT_RUN`; bu görünümden yürütme sonucu bilinmiyorsa
`INCONCLUSIVE` kalır. Bir sohbet mesajı, kapsam onayı veya modelin “bitti”
sözü görev kabulü üretmez. Yerel benzetim kontrolleri canlı model kalitesi,
temiz Windows kurulumu veya üretim kabulü olarak gösterilmez.
