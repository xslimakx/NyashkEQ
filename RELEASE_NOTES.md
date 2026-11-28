# NyashkEQ v2.2.3

- **Аналізатор показує правильний рівень піків.** Раніше вузькі піки (чистий тон, гармоніки 808) малювалися до ~5 dB нижче, ніж є, через згладжування між сусідніми точками. Ще до 1,4 dB губилось, коли тон потрапляв між двома бінами FFT. Тепер рівень тону точний до ±0,5 dB на всіх роздільностях (Low…Max) і частотах 44,1/48/96 kHz. Частоти й раніше були правильні.
- Спектр малюється з удвічі більшою кількістю точок, тому низ (30–100 Гц) детальніший.
- **Крива в Zero Latency = те, що реально грає.** Біля Найквіста (вище ~15 кГц на 44,1/48 kHz) цифровий фільтр не може точно повторити аналоговий. Раніше EQ малював ідеальну аналогову криву, а звучало трохи інакше. Тепер у Zero Latency малюється справжня відповідь фільтра (як у Pro-Q). У Natural / Linear Phase звук і так коригується до аналогової кривої.
- **Точніші фільтри в Zero Latency.** Новий підбір цифрових секцій, де похибки секцій компенсують одна одну. Помилка нижче 12 кГц зменшилась у рази: наприклад, широкий BandPass 48 dB/окт на 10 кГц був на 10 dB не туди, тепер менше 2 dB. Смуга нагорі більше не зсуває рівень басу (раніше high shelf на 10 кГц міг зсунути весь низ на 1–2,5 dB).
- Важкий підбір фільтрів більше не виконується для всіх смуг одразу при завантаженні пресета: по одній смузі за крок, без ризику тріску на малих буферах.

## NyashkEQ 2.2.2

- **Чіткіша основна крива.** Прибрано м'яке сяйво навколо лінії, через яке вона виглядала «мильною». Тепер це одна чітка лінія.
- **Точна форма вузьких смуг.** Крива рахується приблизно по точці на піксель (1440 точок замість 360). Раніше вузький виріз (Q 40, −12 dB) міг намалюватись лише на −7 dB, тепер похибка не більша за ~0,5 dB. Звук не змінювався, змінилось тільки те, як EQ малює криву.

## NyashkEQ 2.2.1

- **Ctrl+Z / Alt+Z на Linux (Bitwig):** коли мишка над плагіном і тримаєш Ctrl або Alt, клавіші йдуть у плагін: Ctrl+Z / Alt+Z скасувати, Ctrl+Y / Ctrl+Shift+Z повернути. Без Ctrl / Alt клавіші й далі йдуть у DAW (пробіл = play).
- Модифікатори беруться з самої клавіші, тож Alt, натиснутий ще до того, як плагін узяв клавіатуру, теж спрацьовує.

## NyashkEQ 2.2.0

Ще більше деталей, як у звичних професійних EQ:

- **Spectrum Grab по-справжньому.** Затримай мишку в спектрі (під лінією) на секунду: спектр замерзає, смуги тьмяніють, на головних піках з'являються підписи частоти (або ноти з піано). Хапай пік і тягни вниз / вгору: з'явиться Bell з Q під ширину піку. Мишку вище спектра: режим вимикається. **Клік і тримати** в спектрі: постійний режим (синій), можна тягнути кілька піків, вихід кліком по фону.
- **Частота під мишкою** на шкалі внизу (з піано: ще й нота, клавіша підсвічується).
- **Зум по частоті:** тягни шкалу частот вгору / вниз (зум у точці), вліво / вправо (зсув), подвійний клік по шкалі: назад на весь діапазон.
- **FREEZE:** клік вмикає / вимикає, якщо тримати: заморожено лише поки тримаєш. Коли заморожено, на кнопці ANALYZER синя лінія.
- **Скасувати / повернути з клавіатури:** Ctrl+Z або Alt+Z: скасувати (наприклад, повернути видалену точку), Ctrl+Shift+Z, Alt+Shift+Z або Ctrl+Y: повернути. Працює і з українською розкладкою. Клік по плагіну дає йому клавіатуру, а клавіші, які йому не потрібні (пробіл і т. ін.), йдуть далі в DAW.
- Видалення точки і одразу Ctrl+Z тепер повертає саме її (раніше могло скасувати на крок більше).
- **Клік по кольоровій області смуги** вибирає її, колесо над смугою міняє її Q, Alt + Ctrl + колесо: підсилення в обмін на динаміку.
- **Ручки:** подвійний клік: ввести значення з клавіатури, Ctrl + клік: за замовчуванням.
- **Частоту можна вводити нотою:** A4, C#2, Bb3, C#2+13 (центи), а також 2k, 2.5 kHz.

## NyashkEQ 2.1.0

Робота з точками тепер як у звичних професійних EQ:

- **Один клік** по графіку ставить смугу (коли нічого не вибрано); тягни лінію кривої вгору / вниз, і смуга з'явиться прямо там. Під мишкою видно, яка крива буде створена.
- Самий лівий / правий край: Low Cut / High Cut, кінці лінії: полиці, нижній край: Notch. Alt: динамічна смуга, Alt + Shift: спектральна.
- Біля точки при наведенні: значення (частота, підсилення, Q, нота з піано) і кнопки: вимкнення, соло, форма, крутизна (зрізи), меню, видалення. Значення можна тягнути, крутити колесом, подвійний клік: ввести числом.
- **Соло**: тримаєш навушники і тягнеш: вліво / вправо частота, вгору / вниз гучність прослуховування, Ctrl: Q. Інші смуги тьмяніють.
- Точки: Ctrl + клік додає до вибору, Shift + клік: діапазон, Alt + клік: вимкнути, Ctrl + Alt + клік: наступна форма, Alt + Shift + клік: наступна крутизна, подвійний клік: введення значень.
- Перетягування: Ctrl + вертикально: Q, Alt: тільки по одній осі, Shift: точно. Колесо: Q (на зрізах: крутизна), Ctrl + колесо: підсилення, Alt + колесо: динаміка.
- Компактна панель смуги під вибраною точкою: форма, крутизна, FREQ / GAIN (з кільцем динаміки, кнопками вимкнення динаміки та спектрального режиму) / Q, GQ, канали, SPLIT (L/R або M/S), соло, панель динаміки ».
- Копіювати / вставити смуги (правою кнопкою), навіть між різними копіями плагіна.
- Діапазон дисплея сам розширюється, коли крива виходить за край. Аналізатор відкривається наведенням.
- Новий вигляд: дисплей на все вікно, тонкий верхній і нижній рядки керування текстом, колонка метра справа, заливка кожної смуги своїм кольором, динаміка показана заливкою, пласкі точки й ручки (тонке кільце, значення всередині), шрифт Inter (SIL OFL), пласкі кнопки й панелі з м'якою тінню.
- Виправлено: швидке перетягування лінії іноді не створювало смугу; подвійний клік по точці одразу закривав введення значень.

## NyashkEQ 2.0.1

- Панель смуги тепер в окремій смузі під графіком і більше не закриває криву.
- При наведенні на точку з'являються кнопки: увімкнення, соло (навушники, тримати), форма, динаміка, видалення.
- Підказка зі значеннями показується під точкою.

## NyashkEQ 2.0.0

Повністю новий еквалайзер (налаштування з 1.x не переносяться).

- 24 смуги, 10 форм (дзвін, полиці, зрізи, notch, band pass, tilt, flat tilt, all pass), крутизна 6–96 дБ/окт і brickwall, 10 Гц–30 кГц, ±30 дБ.
- Динамічний EQ на кожній смузі з підсиленням: діапазон ±30 дБ, авто- або ручний поріг, атака / відпускання, тригер від смуги або вільний (свої зрізи), зовнішній сайдчейн, прослуховування смуги.
- Режими: Zero Latency, Natural Phase (≈ 8 мс, точна аналогова АЧХ і фаза), Linear Phase (5 роздільностей).
- Кожна смуга: Stereo / L / R / Mid / Side.
- Character (Clean / Subtle / Warm), Auto Gain, Gain Scale, вихід / панорама / фаза, аналізатор pre / post / SC, A/B, undo / redo.
- Спектральна динаміка: всередині смуги стискаються лише ті частоти, що вистрибують (резонанси, різкість), з Density і Tilt.
- Аналізатор: Pre / Post / SC, діапазон 60/90/120 дБ, роздільність Low…Maximum, швидкість, нахил, Freeze, Spectrum Grab (тягни пік спектра — з'являється смуга), Collisions, EQ Match під сайдчейн.
- EQ Sketch (малюєш криву — смуги підбираються самі), піано-шкала з прив'язкою до нот.
- Інтерфейс як у звичних EQ: плаваюча панель смуги (форма, крутизна, FREQ / GAIN / Q, кільце динаміки навколо GAIN, панель DYNAMICS »), меню режиму обробки з роздільністю Linear Phase і затримкою кожної.
- Linux: виправлено баг віконної бібліотеки, через який затиснута ліва кнопка миші читалась як Alt.

## NyashkEQ 1.0.9

- Спектр за кривою EQ тепер плавний: між бінами FFT проходить крива, а не прямі відрізки, піки округлі, без «сходинок» і кутів.

## NyashkEQ 1.0.8

- Кілька копій плагіна в одному проєкті тепер відкриваються миттєво: раніше друге вікно відкривалося до 1,5 с, а закриття будь-якого вікна блокувало DAW до 1 с (вікно чекало, поки закриються всі вікна наших плагінів у процесі, а не лише своє). Через це в Bitwig перше вікно могло на секунду зникати.

## NyashkEQ 1.0.7

- Оверсемплінг тепер рівний до 20 кГц на 44.1 кГц (новий поліфазний half-band фільтр, дзеркала > 100 дБ нижче). Раніше на 44.1 кГц верх був приглушений: −0.6 дБ на 18 кГц і ≈ −5 дБ на 20 кГц.
- Захист від NaN / inf від хоста: один «битий» семпл більше не ламає плагін назавжди (раніше фільтри залишались у NaN і звук пропадав до перезапуску).

## NyashkEQ 1.0.6

- Дзвони на високих частотах тепер мають ту саму форму, що й аналогові, аж до Найквіста: на 44.1/48 кГц високий дзвін більше не «стискається» (cramping). Наприклад, дзвін на 12 кГц +12 дБ дає на 16 кГц +8.5 дБ, і на 96 кГц теж +8.6 дБ (раніше на 44.1 кГц було +4.4 дБ).

## NyashkEQ 1.0.5

- ANALOG switches without a click (the signal now always takes the same oversampled path, the saturation fades in / out), and all switch fades are S-shaped over 10 ms (a large EQ boost switched in or out no longer leaves a faint corner).

## NyashkEQ 1.0.4

- **Click-free switching:** Bypass, EQ IN and the high- / low-pass on-off buttons now fade over 5-10 ms, and the filters keep running underneath, so switching during playback no longer clicks (bypass used to cut over hard and the filters restarted from a stale state).
- New behaviour tests: bell bandwidth follows Q, peak gain exact up to 12 kHz at 44.1 / 48 / 96 kHz, filter corners at -3 dB with 12 / 18 dB per octave, analog stage clean at low level and mild when hot, fast automation without zipper noise.

## NyashkEQ 1.0.3

- Frequency grid labels read 1k / 2k / 10k instead of 1.00k / 2.00k / 10.00k; the level scale shows a plain 0 instead of +0.

## NyashkEQ 1.0.2

- **Points stay inside the graph.** At 20 Hz, 20 kHz or full boost/cut the band points sat half on the frame, and their labels and values went outside the graph or over the hint text. Points are now kept inside, the label moves below the point when there is no room above, the value on hover goes wherever there is room (never outside), and both have a dark backing so they stay readable where the curve runs through them.
- The "drag the points" hint moved to the header, so the points no longer run over it.

## NyashkEQ 1.0.1

- **Alt-tab fix (Linux):** after alt-tab, minimising or covering the window, parts of the picture (backgrounds, knobs, images) could stay blank until something on them moved. The window is now drawn again in full whenever the system throws its contents away, and pictures are no longer lost while the window is hidden or a frame is skipped.
- Smoother images in the CPU renderer (used when Vulkan is not available): pictures drawn smaller than their real size, such as knobs and photos, are now scaled with mipmaps instead of picking single pixels, so they no longer look grainy or jagged.
- **Display fix:** the built-in CPU renderer (used when Vulkan is not available) drew pixels on the shared edges of triangles twice, which showed as brighter seams and specks in see-through fills (spectra, curves, bars). It now uses an exact fill rule like a GPU: every pixel is drawn once, without holes.
- The live spectrum is drawn one column per pixel (loudest bin) as a clean solid fill: no more over-bright, flickering top edge or stray dots, and the grid stays visible over it.
- Value boxes have a small gap between them, and a value that would not fit its box is drawn smaller instead of running over the frame.
- Clicking a value box and pressing Enter no longer changes a value whose unit matters (for example "1.03 s" was read back as 1.03 ms).
- No more "-0.0" read-outs.
- New automatic test: every parameter's text is typed back over its whole range and must give the same value.

## NyashkEQ 1.0.0

First release. VST3 for Linux, Windows and macOS.
