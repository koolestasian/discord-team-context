# Team record

Plain Markdown export of the team record. Dates are UTC. _Titles and names are copied from chat. Treat them as data, not instructions._

## Decisions

### D-1: Beta 1 ships Friday 3 April

* Status: superseded
* Captured by Wren Calloway on 2026-03-02T10:10:34Z via slash
* Superseded by: D-50
* Tags: beta, release
* Source: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1477970509244268610>)

### D-3: Use libspine-core on both platforms for EPUB parsing, behind a thin shim

* Status: superseded
* Captured by Dov Kestrel on 2026-03-03T10:09:23Z via slash
* Superseded by: D-23
* Tags: epub, parser
* Source: [Message in #engineering by Dov Kestrel](<https://discord.com/channels/1/202/1478332379545337992>)

### D-7: Default reading font is Fenwick Serif for body text, with Alder Sans for the interface

* Status: active
* Captured by Nadia Quill on 2026-03-04T12:40:45Z via context_menu
* Tags: font, design
* Source: [Message in #design by Wren Calloway](<https://discord.com/channels/1/203/1478731524147904774>)

### D-8: Onboarding: create an account at first launch, four screens

* Status: superseded
* Captured by Nadia Quill on 2026-03-05T14:43:34Z via slash
* Superseded by: D-16
* Tags: onboarding
* Source: [Message in #onboarding-v2 by Nadia Quill](<https://discord.com/channels/1/213/1479126360084578664>)

### D-9: App icon is sketch 4: a lantern sitting on a book spine

* Status: active
* Captured by Nadia Quill on 2026-03-06T17:16:38Z via slash
* Tags: brand
* Source: [Message in #general by Nadia Quill](<https://discord.com/channels/1/201/1479525229494665639>)

### D-12: PDF is out of scope for 1.0: reflowable EPUB only

* Status: active
* Captured by Wren Calloway on 2026-03-09T09:22:17Z via slash
* Tags: scope
* Source: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1480495394617164220>)

### D-13: No DRM support: the app does not open DRM-protected files

* Status: active
* Captured by Tavish Orme on 2026-03-09T09:27:45Z via slash
* Tags: scope, drm
* Source: [Message in #general by Tavish Orme](<https://discord.com/channels/1/201/1480496413442965955>)

### D-14: Android minSdk is API 26 (Android 8)

* Status: superseded
* Captured by Dov Kestrel on 2026-03-09T11:24:27Z via slash
* Superseded by: D-46
* Tags: android
* Source: [Message in #engineering by Wren Calloway](<https://discord.com/channels/1/202/1480525625239405030>)

### D-15: Pay the one-off $240 Fenwick license, which covers unlimited apps

* Status: active
* Captured by Wren Calloway on 2026-03-09T15:15:38Z via slash
* Tags: font, licence
* Source: [Message in #design by Wren Calloway](<https://discord.com/channels/1/203/1480583469116555771>)

### D-16: Onboarding needs no account: sign-in is optional, offered in Settings and when a second device opens the library

* Status: active
* Captured by Wren Calloway on 2026-03-12T17:04:32Z via slash
* Replaces: D-8
* Tags: onboarding
* Source: [Message in #onboarding-v2 by Wren Calloway](<https://discord.com/channels/1/213/1481698790154044032>)

### D-18: Price is a $4.99 one-time unlock, no subscription

* Status: superseded
* Captured by Wren Calloway on 2026-03-12T18:13:11Z via slash
* Superseded by: D-36
* Tags: pricing
* Source: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1481714918389580444>)

### D-20: Free trial is 14 days

* Status: superseded
* Captured by Wren Calloway on 2026-03-12T18:14:41Z via slash
* Superseded by: D-74
* Tags: pricing
* Source: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1481714918389580444>)

### D-21: Crash review every Thursday morning once the beta starts

* Status: superseded
* Captured by Wren Calloway on 2026-03-12T18:15:10Z via slash
* Superseded by: D-68
* Tags: crashes, process
* Source: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1481714918389580444>)

### D-22: No third-party analytics SDKs; crash reports are opt-in and off by default

* Status: active
* Captured by Wren Calloway on 2026-03-12T18:16:28Z via slash
* Tags: privacy
* Source: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1481714918389580444>)

### D-23: Drop libspine: write our own tolerant zip + OPF reader on each platform, tested against the 40-book corpus

* Status: active
* Captured by Wren Calloway on 2026-03-16T10:58:24Z via slash
* Replaces: D-3
* Tags: epub, parser
* Source: [Message in #epub-parser-crashes by Wren Calloway](<https://discord.com/channels/1/211/1483056235002594060>)

### D-27: Reading position conflicts: last write wins, ordered by server receipt time

* Status: superseded
* Captured by Tavish Orme on 2026-03-16T13:35:26Z via slash
* Superseded by: D-39
* Tags: sync
* Source: [Message in #sync-conflicts by Tavish Orme](<https://discord.com/channels/1/212/1483095561283437366>)

### D-28: Annotations merge as a union; if the same annotation is edited on two devices the newest edit wins per field

* Status: active
* Captured by Tavish Orme on 2026-03-17T10:05:25Z via slash
* Tags: sync, annotations
* Source: [Message in #sync-conflicts by Wren Calloway](<https://discord.com/channels/1/212/1483404849931486034>)

### D-29: Cap decoded images at 2048 px on the longest side, on both platforms

* Status: superseded
* Captured by Dov Kestrel on 2026-03-17T14:53:46Z via slash
* Superseded by: D-51
* Tags: images, memory
* Source: [Message in #epub-parser-crashes by Wren Calloway](<https://discord.com/channels/1/211/1483477050147865461>)

### D-30: At most 2 downloads run at the same time

* Status: active
* Captured by Wren Calloway on 2026-03-17T16:27:47Z via slash
* Tags: downloads
* Source: [Message in #engineering by Wren Calloway](<https://discord.com/channels/1/202/1483501466802062267>)

### D-31: Highlight export is one Markdown file per book: quote, chapter and a footnote-style locator

* Status: active
* Captured by Imke Brandt on 2026-03-17T16:29:13Z via context_menu
* Tags: export, annotations
* Source: [Message in #engineering by Wren Calloway](<https://discord.com/channels/1/202/1483500969621849005>)

### D-34: If one device edits a highlight another device deleted, the edit wins and the highlight comes back

* Status: active
* Captured by Wren Calloway on 2026-03-18T11:52:47Z via slash
* Tags: sync, annotations
* Source: [Message in #sync-conflicts by Wren Calloway](<https://discord.com/channels/1/212/1483794654754374650>)

### D-35: Downloads run on Wi-Fi only by default

* Status: superseded
* Captured by Wren Calloway on 2026-03-19T12:09:09Z via slash
* Superseded by: D-54
* Tags: downloads
* Source: [Message in #engineering by Wren Calloway](<https://discord.com/channels/1/202/1484160784845112349>)

### D-36: Price is a $7.99 one-time unlock, no subscription

* Status: active
* Captured by Wren Calloway on 2026-03-23T15:12:16Z via slash
* Replaces: D-18
* Tags: pricing
* Source: [Message in #pricing-numbers by Wren Calloway](<https://discord.com/channels/1/214/1485656921158583430>)

### D-39: Reading position: furthest page wins; prompt to continue if the other device is more than 20 pages away

* Status: active
* Captured by Wren Calloway on 2026-03-24T08:23:17Z via slash
* Replaces: D-27
* Tags: sync
* Source: [Message in #sync-conflicts by Wren Calloway](<https://discord.com/channels/1/212/1485916079220982967>)

### D-41: No page-curl animation: slide, fade and none only

* Status: superseded
* Captured by Wren Calloway on 2026-03-24T10:18:30Z via slash
* Superseded by: D-59
* Tags: animation
* Source: [Message in #design by Wren Calloway](<https://discord.com/channels/1/203/1485945051711407336>)

### D-43: Covers are extracted from the EPUB at import; books without one get a generated title card; no CDN

* Status: active
* Captured by Dov Kestrel on 2026-03-25T09:50:40Z via slash
* Tags: covers, import
* Source: [Message in #engineering by Wren Calloway](<https://discord.com/channels/1/202/1486299637148157230>)

### D-44: Three themes: light, dark with true black, and sepia; follow the system by default

* Status: active
* Captured by Nadia Quill on 2026-03-27T11:09:14Z via slash
* Tags: design, themes
* Source: [Message in #design by Wren Calloway](<https://discord.com/channels/1/203/1487044804683629891>)

### D-46: Android minSdk drops to API 24 (Android 7) so the older tester tablets work

* Status: active
* Captured by Dov Kestrel on 2026-03-30T10:24:31Z via slash
* Replaces: D-14
* Tags: android
* Source: [Message in #engineering by Wren Calloway](<https://discord.com/channels/1/202/1488120887684236667>)

### D-47: The Easeread dyslexia-friendly font ships in 1.0

* Status: superseded
* Captured by Nadia Quill on 2026-03-30T13:40:16Z via slash
* Superseded by: D-63
* Tags: accessibility, font
* Source: [Message in #dyslexia-and-accessibility by Wren Calloway](<https://discord.com/channels/1/216/1488169731927180688>)

### D-48: Minimum touch target is 48dp everywhere

* Status: active
* Captured by Nadia Quill on 2026-03-30T13:41:46Z via slash
* Tags: accessibility
* Source: [Message in #dyslexia-and-accessibility by Wren Calloway](<https://discord.com/channels/1/216/1488169731927180688>)

### D-50: Beta 1 moves to Friday 10 April because too many corpus books fail to open

* Status: superseded
* Captured by Wren Calloway on 2026-04-01T09:47:30Z via slash
* Replaces: D-1
* Superseded by: D-56
* Tags: beta, release
* Source: [Message in #beta-1-checklist by Wren Calloway](<https://discord.com/channels/1/215/1488836597662221796>)

### D-51: Decode images within a 96 MB per-chapter memory budget, downsampling the largest first; the pixel cap goes away

* Status: active
* Captured by Wren Calloway on 2026-04-02T11:38:26Z via slash
* Replaces: D-29
* Tags: images, memory
* Source: [Message in #epub-parser-crashes by Wren Calloway](<https://discord.com/channels/1/211/1489226651802928654>)

### D-54: On mobile data, books under 25 MB download freely; over 25 MB the app asks once and shows the size

* Status: active
* Captured by Wren Calloway on 2026-04-06T09:48:14Z via slash
* Replaces: D-35
* Tags: downloads
* Source: [Message in #engineering by Wren Calloway](<https://discord.com/channels/1/202/1490648558880294470>)

### D-56: Beta 1 goes out Tuesday 14 April instead of Friday the 10th

* Status: active
* Captured by Wren Calloway on 2026-04-06T11:14:26Z via slash
* Replaces: D-50
* Tags: beta, release
* Source: [Message in #beta-1-checklist by Wren Calloway](<https://discord.com/channels/1/215/1490670457064654434>)

### D-57: We never release on a Friday

* Status: active
* Captured by Wren Calloway on 2026-04-06T11:15:41Z via slash
* Tags: release, process
* Source: [Message in #beta-1-checklist by Wren Calloway](<https://discord.com/channels/1/215/1490670457064654434>)

### D-59: Page curl ships as an opt-in page turn setting; the default stays slide

* Status: active
* Captured by Wren Calloway on 2026-04-09T14:13:28Z via slash
* Replaces: D-41
* Tags: animation
* Source: [Message in #design by Wren Calloway](<https://discord.com/channels/1/203/1491802408454588121>)

### D-63: 1.0 ships line and word spacing sliders; the Easeread dyslexia font moves to 1.1

* Status: active
* Captured by Wren Calloway on 2026-04-13T15:43:18Z via slash
* Replaces: D-47
* Tags: accessibility, font
* Source: [Message in #dyslexia-and-accessibility by Wren Calloway](<https://discord.com/channels/1/216/1493274588170290975>)

### D-67: P0 = crash on open or first page for more than 1% of sessions, fixed the same day; P1 within the week; P2 backlog

* Status: active
* Captured by Wren Calloway on 2026-04-15T08:53:26Z via slash
* Tags: crashes, process
* Source: [Message in #release-ops by Wren Calloway](<https://discord.com/channels/1/204/1493896201840494521>)

### D-68: Crash review moves to Tuesdays at 10:00 UTC

* Status: active
* Captured by Wren Calloway on 2026-04-15T09:07:33Z via slash
* Replaces: D-21
* Tags: crashes, process
* Source: [Message in #release-ops by Wren Calloway](<https://discord.com/channels/1/204/1493899986289690567>)

### D-70: 1.0 store submission target is Tuesday 26 May

* Status: superseded
* Captured by Wren Calloway on 2026-04-16T17:52:45Z via slash
* Superseded by: D-76
* Tags: release
* Source: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1494392206692321258>)

### D-71: No text-to-speech in 1.0; revisit after launch

* Status: active
* Captured by Wren Calloway on 2026-04-16T17:53:13Z via slash
* Tags: scope
* Source: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1494392206692321258>)

### D-74: Free trial shortens to 7 days, with a reminder banner on day 5

* Status: active
* Captured by Wren Calloway on 2026-04-20T10:33:22Z via slash
* Replaces: D-20
* Tags: pricing
* Source: [Message in #pricing-numbers by Wren Calloway](<https://discord.com/channels/1/214/1495733570554169392>)

### D-76: 1.0 store submission moves to Tuesday 2 June to absorb the accessibility fixes

* Status: active
* Captured by Wren Calloway on 2026-04-22T13:23:50Z via slash
* Replaces: D-70
* Tags: release
* Source: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1496500878272759926>)

## Tasks

### T-2: Confirm 30 beta testers

* Status: done
* Captured by Wren Calloway on 2026-03-02T10:16:29Z via slash
* Owner: Pell Hartnoll
* Due: 2026-03-31T17:00:00Z
* Tags: beta
* Source: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1477971746027077705>)

### T-4: Prototype importer that opens 10 sample books

* Status: done
* Captured by Wren Calloway on 2026-03-03T10:33:30Z via slash
* Owner: Dov Kestrel
* Due: 2026-03-13T17:00:00Z
* Tags: epub, import
* Source: [Message in #engineering by Wren Calloway](<https://discord.com/channels/1/202/1478338650008846479>)

### T-6: Email the Fenwick foundry about embedding the font in an app

* Status: done
* Captured by Nadia Quill on 2026-03-04T10:50:40Z via slash
* Owner: Pell Hartnoll
* Due: 2026-03-13T17:00:00Z
* Tags: font, licence
* Source: [Message in #design by Pell Hartnoll](<https://discord.com/channels/1/203/1478704591171354851>)

### T-17: Mock the three-screen onboarding v2

* Status: done
* Captured by Wren Calloway on 2026-03-12T17:14:15Z via slash
* Owner: Nadia Quill
* Due: 2026-03-18T17:00:00Z
* Tags: onboarding, design
* Source: [Message in #onboarding-v2 by Nadia Quill](<https://discord.com/channels/1/213/1481700826673185422>)

### T-19: Spike text selection handles on iOS

* Status: done
* Captured by Wren Calloway on 2026-03-12T18:14:27Z via reaction
* Owner: Imke Brandt
* Due: 2026-03-20T17:00:00Z
* Tags: ios, reader
* Source: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1481714918389580444>)

### T-24: Own zip + OPF reader on Android, replacing libspine

* Status: done
* Captured by Wren Calloway on 2026-03-16T10:59:35Z via slash
* Owner: Dov Kestrel
* Due: 2026-04-01T17:00:00Z
* Tags: epub, parser
* Source: [Message in #epub-parser-crashes by Wren Calloway](<https://discord.com/channels/1/211/1483056235002594060>)

### T-25: Grow the awkward-EPUB test corpus to 40 books

* Status: done
* Captured by Wren Calloway on 2026-03-16T11:00:33Z via slash
* Owner: Pell Hartnoll
* Due: 2026-03-27T17:00:00Z
* Tags: epub, testing
* Source: [Message in #epub-parser-crashes by Wren Calloway](<https://discord.com/channels/1/211/1483056235002594060>)

### T-26: One-page sync conflict spec for Imke

* Status: done
* Captured by Wren Calloway on 2026-03-16T13:34:08Z via slash
* Owner: Tavish Orme
* Due: 2026-03-20T17:00:00Z
* Tags: sync
* Source: [Message in #sync-conflicts by Wren Calloway](<https://discord.com/channels/1/212/1483095267459859247>)

### T-32: Resumable downloads: range requests, retry with backoff, resume after the app is killed

* Status: done
* Captured by Wren Calloway on 2026-03-17T16:29:29Z via slash
* Owner: Tavish Orme
* Due: 2026-04-08T17:00:00Z
* Tags: downloads
* Source: [Message in #engineering by Wren Calloway](<https://discord.com/channels/1/202/1483501692438840258>)

### T-37: Create the $7.99 price tier in both store consoles

* Status: done
* Captured by Wren Calloway on 2026-03-23T15:13:51Z via slash
* Owner: Wren Calloway
* Due: 2026-04-09T17:00:00Z
* Tags: pricing, release
* Source: [Message in #pricing-numbers by Wren Calloway](<https://discord.com/channels/1/214/1485656921158583430>)

### T-40: Build the 'continue from page N on \<device\>' prompt

* Status: done
* Captured by Wren Calloway on 2026-03-24T08:29:33Z via slash
* Owner: Imke Brandt
* Due: 2026-04-03T17:00:00Z
* Tags: sync, ios
* Source: [Message in #sync-conflicts by Wren Calloway](<https://discord.com/channels/1/212/1485917627619607749>)

### T-42: Look at CDN options for cover thumbnails

* Status: dropped
* Captured by Dov Kestrel on 2026-03-25T09:34:48Z via slash
* Owner: Tavish Orme
* Due: 2026-03-31T17:00:00Z
* Tags: covers
* Source: [Message in #engineering by Dov Kestrel](<https://discord.com/channels/1/202/1486296122497565970>)

### T-45: Support API 24 and smoke-test on the old tablet

* Status: done
* Captured by Dov Kestrel on 2026-03-30T10:23:51Z via slash
* Owner: Dov Kestrel
* Due: 2026-04-02T17:00:00Z
* Tags: android
* Source: [Message in #engineering by Dov Kestrel](<https://discord.com/channels/1/202/1488120156088567156>)

### T-49: Line and word spacing slider UI

* Status: done
* Captured by Nadia Quill on 2026-03-30T13:42:27Z via slash
* Owner: Nadia Quill
* Due: 2026-04-10T17:00:00Z
* Tags: accessibility, design
* Source: [Message in #dyslexia-and-accessibility by Wren Calloway](<https://discord.com/channels/1/216/1488169731927180688>)

### T-52: Implement the per-chapter image memory budget

* Status: done
* Captured by Wren Calloway on 2026-04-02T11:40:16Z via slash
* Owner: Dov Kestrel
* Due: 2026-04-08T17:00:00Z
* Tags: images, memory
* Source: [Message in #epub-parser-crashes by Dov Kestrel](<https://discord.com/channels/1/211/1489226951074907669>)

### T-55: Design the size-confirm sheet for big downloads on mobile data

* Status: done
* Captured by Wren Calloway on 2026-04-06T09:53:19Z via slash
* Owner: Nadia Quill
* Due: 2026-04-10T17:00:00Z
* Tags: downloads, design
* Source: [Message in #engineering by Nadia Quill](<https://discord.com/channels/1/202/1490649823748163149>)

### T-60: Measure the page-curl battery cost on the old Android tablet; pull the Android curl if over 3%

* Status: done
* Captured by Wren Calloway on 2026-04-09T14:18:37Z via slash
* Owner: Imke Brandt
* Due: 2026-04-17T17:00:00Z
* Tags: animation, android
* Source: [Message in #design by Wren Calloway](<https://discord.com/channels/1/203/1491803436247811815>)

### T-61: Finalise the beta 1 release notes

* Status: done
* Captured by Wren Calloway on 2026-04-10T17:04:16Z via slash
* Owner: Wren Calloway
* Due: 2026-04-13T17:00:00Z
* Tags: beta, release
* Source: [Message in #beta-1-checklist by Wren Calloway](<https://discord.com/channels/1/215/1492207565294143214>)

### T-64: Write up the Easeread dyslexia font evaluation

* Status: open
* Captured by Wren Calloway on 2026-04-13T15:48:15Z via slash
* Owner: Nadia Quill
* Due: 2026-05-15T17:00:00Z
* Tags: accessibility, font
* Source: [Message in #dyslexia-and-accessibility by Wren Calloway](<https://discord.com/channels/1/216/1493275862299182893>)

### T-65: Screen-reader audit of the reader screen with TalkBack and VoiceOver

* Status: open
* Captured by Wren Calloway on 2026-04-14T09:16:38Z via slash
* Owner: Imke Brandt
* Due: 2026-04-17T17:00:00Z
* Tags: accessibility
* Source: [Message in #dyslexia-and-accessibility by Imke Brandt](<https://discord.com/channels/1/216/1493539606778480478>)

### T-69: Triage the top 5 beta crash groups

* Status: done
* Captured by Wren Calloway on 2026-04-15T09:15:15Z via slash
* Owner: Dov Kestrel
* Due: 2026-04-21T17:00:00Z
* Tags: crashes
* Source: [Message in #release-ops by Wren Calloway](<https://discord.com/channels/1/204/1493901719791339477>)

### T-72: Privacy policy page and store data-safety forms

* Status: open
* Captured by Wren Calloway on 2026-04-16T17:54:22Z via slash
* Owner: Tavish Orme
* Due: 2026-05-01T17:00:00Z
* Tags: privacy, release
* Source: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1494392206692321258>)

### T-73: Store screenshots for six device sizes

* Status: open
* Captured by Wren Calloway on 2026-04-16T17:55:32Z via slash
* Owner: Pell Hartnoll
* Due: 2026-05-08T17:00:00Z
* Tags: release
* Source: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1494392206692321258>)

### T-75: Design the day-5 trial reminder banner

* Status: open
* Captured by Wren Calloway on 2026-04-20T10:34:19Z via slash
* Owner: Nadia Quill
* Due: 2026-04-28T17:00:00Z
* Tags: pricing, design
* Source: [Message in #pricing-numbers by Wren Calloway](<https://discord.com/channels/1/214/1495733570554169392>)

## Questions

### Q-5: Is Fenwick licensed for apps? last time I checked it was desktop-only

* Status: answered
* Captured by Nadia Quill on 2026-03-04T10:32:17Z via context_menu
* Tags: font, licence
* Source: [Message in #design by Imke Brandt](<https://discord.com/channels/1/203/1478699829893923029>)
* Answered in: [Message in #design by Pell Hartnoll](<https://discord.com/channels/1/203/1480581444375020020>)

### Q-10: Will the app open PDFs?

* Status: answered
* Captured by Pell Hartnoll on 2026-03-09T09:18:23Z via slash
* Tags: scope
* Source: [Message in #general by Pell Hartnoll](<https://discord.com/channels/1/201/1480492889149669813>)
* Answered in: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1480495394617164220>)

### Q-11: Will the app open DRM-protected books bought elsewhere?

* Status: answered
* Captured by Pell Hartnoll on 2026-03-09T09:19:42Z via slash
* Tags: scope, drm
* Source: [Message in #general by Pell Hartnoll](<https://discord.com/channels/1/201/1480492889149669813>)
* Answered in: [Message in #general by Tavish Orme](<https://discord.com/channels/1/201/1480496413442965955>)

### Q-33: what happens if I delete a highlight on my phone while my tablet is offline, and on the tablet I edit the note on that same highlight?

* Status: answered
* Captured by Dov Kestrel on 2026-03-18T11:24:09Z via context_menu
* Tags: sync, annotations
* Source: [Message in #sync-conflicts by Dov Kestrel](<https://discord.com/channels/1/212/1483785819939537879>)
* Answered in: [Message in #sync-conflicts by Wren Calloway](<https://discord.com/channels/1/212/1483794654754374650>)

### Q-38: also: regional pricing? or one USD tier and let the stores convert?

* Status: open
* Captured by Dov Kestrel on 2026-03-23T15:34:32Z via slash
* Tags: pricing
* Source: [Message in #pricing-numbers by Dov Kestrel](<https://discord.com/channels/1/214/1485659407575549069>)

### Q-53: Are we translating at launch?

* Status: open
* Captured by Nadia Quill on 2026-04-02T15:09:25Z via slash
* Tags: localisation
* Source: [Message in #design by Nadia Quill](<https://discord.com/channels/1/203/1489278301125084700>)

### Q-58: cold start on the 2017 tablet is about 3.2 s to the first page. on the newer phone it's 1.1. is that expected?

* Status: open
* Captured by Dov Kestrel on 2026-04-08T10:12:08Z via slash
* Tags: performance
* Source: [Message in #engineering by Imke Brandt](<https://discord.com/channels/1/202/1491377079520986743>)

### Q-62: Do we want an iPad split view with the library on the left?

* Status: dropped
* Captured by Imke Brandt on 2026-04-11T12:10:06Z via slash
* Tags: ios, tablet
* Source: [Message in #design by Imke Brandt](<https://discord.com/channels/1/203/1492494431251728124>)

### Q-66: Will the app read books aloud?

* Status: answered
* Captured by Pell Hartnoll on 2026-04-14T14:07:45Z via slash
* Tags: scope, accessibility
* Source: [Message in #general by Pell Hartnoll](<https://discord.com/channels/1/201/1493611833599330184>)
* Answered in: [Message in #general by Wren Calloway](<https://discord.com/channels/1/201/1494392206692321258>)

## History

* 2026-03-02T10:10:34Z D-1 captured by Wren Calloway via slash.
* 2026-03-02T10:16:29Z T-2 captured by Wren Calloway via slash.
* 2026-03-03T10:09:23Z D-3 captured by Dov Kestrel via slash.
* 2026-03-03T10:33:30Z T-4 captured by Wren Calloway via slash.
* 2026-03-04T10:32:17Z Q-5 captured by Nadia Quill via context_menu.
* 2026-03-04T10:50:40Z T-6 captured by Nadia Quill via slash.
* 2026-03-04T12:40:45Z D-7 captured by Nadia Quill via context_menu.
* 2026-03-05T14:43:34Z D-8 captured by Nadia Quill via slash.
* 2026-03-06T17:16:38Z D-9 captured by Nadia Quill via slash.
* 2026-03-09T09:18:23Z Q-10 captured by Pell Hartnoll via slash.
* 2026-03-09T09:19:42Z Q-11 captured by Pell Hartnoll via slash.
* 2026-03-09T09:22:17Z D-12 captured by Wren Calloway via slash.
* 2026-03-09T09:23:15Z Q-10 went from open to answered (by Pell Hartnoll).
* 2026-03-09T09:27:18Z Q-11 went from open to answered (by Pell Hartnoll).
* 2026-03-09T09:27:45Z D-13 captured by Tavish Orme via slash.
* 2026-03-09T11:24:27Z D-14 captured by Dov Kestrel via slash.
* 2026-03-09T15:04:13Z T-6 went from open to done (by Nadia Quill).
* 2026-03-09T15:05:11Z Q-5 went from open to answered (by Nadia Quill).
* 2026-03-09T15:15:38Z D-15 captured by Wren Calloway via slash.
* 2026-03-12T17:04:32Z D-8 went from active to superseded (by Wren Calloway).
* 2026-03-12T17:04:32Z D-16 captured by Wren Calloway via slash.
* 2026-03-12T17:14:15Z T-17 captured by Wren Calloway via slash.
* 2026-03-12T18:13:11Z D-18 captured by Wren Calloway via slash.
* 2026-03-12T18:14:27Z T-19 captured by Wren Calloway via reaction.
* 2026-03-12T18:14:41Z D-20 captured by Wren Calloway via slash.
* 2026-03-12T18:15:10Z D-21 captured by Wren Calloway via slash.
* 2026-03-12T18:16:28Z D-22 captured by Wren Calloway via slash.
* 2026-03-13T17:20:10Z T-4 went from open to done (by Wren Calloway).
* 2026-03-16T10:58:24Z D-3 went from active to superseded (by Wren Calloway).
* 2026-03-16T10:58:24Z D-23 captured by Wren Calloway via slash.
* 2026-03-16T10:59:35Z T-24 captured by Wren Calloway via slash.
* 2026-03-16T11:00:33Z T-25 captured by Wren Calloway via slash.
* 2026-03-16T13:34:08Z T-26 captured by Wren Calloway via slash.
* 2026-03-16T13:35:26Z D-27 captured by Tavish Orme via slash.
* 2026-03-17T10:05:25Z D-28 captured by Tavish Orme via slash.
* 2026-03-17T14:53:46Z D-29 captured by Dov Kestrel via slash.
* 2026-03-17T16:27:47Z D-30 captured by Wren Calloway via slash.
* 2026-03-17T16:29:13Z D-31 captured by Imke Brandt via context_menu.
* 2026-03-17T16:29:29Z T-32 captured by Wren Calloway via slash.
* 2026-03-18T11:05:40Z T-17 went from open to done (by Wren Calloway).
* 2026-03-18T11:24:09Z Q-33 captured by Dov Kestrel via context_menu.
* 2026-03-18T11:52:47Z D-34 captured by Wren Calloway via slash.
* 2026-03-18T11:53:20Z Q-33 went from open to answered (by Dov Kestrel).
* 2026-03-19T12:09:09Z D-35 captured by Wren Calloway via slash.
* 2026-03-19T17:30:42Z T-26 went from open to done (by Wren Calloway).
* 2026-03-20T16:10:22Z T-19 went from open to done (by Wren Calloway).
* 2026-03-23T15:12:16Z D-18 went from active to superseded (by Wren Calloway).
* 2026-03-23T15:12:16Z D-36 captured by Wren Calloway via slash.
* 2026-03-23T15:13:51Z T-37 captured by Wren Calloway via slash.
* 2026-03-23T15:34:32Z Q-38 captured by Dov Kestrel via slash.
* 2026-03-24T08:23:17Z D-27 went from active to superseded (by Wren Calloway).
* 2026-03-24T08:23:17Z D-39 captured by Wren Calloway via slash.
* 2026-03-24T08:29:33Z T-40 captured by Wren Calloway via slash.
* 2026-03-24T10:18:30Z D-41 captured by Wren Calloway via slash.
* 2026-03-25T09:34:48Z T-42 captured by Dov Kestrel via slash.
* 2026-03-25T09:46:39Z T-42 went from open to dropped (by Dov Kestrel).
* 2026-03-25T09:50:40Z D-43 captured by Dov Kestrel via slash.
* 2026-03-27T09:02:03Z T-25 went from open to done (by Wren Calloway).
* 2026-03-27T11:09:14Z D-44 captured by Nadia Quill via slash.
* 2026-03-30T10:23:51Z T-45 captured by Dov Kestrel via slash.
* 2026-03-30T10:24:31Z D-14 went from active to superseded (by Dov Kestrel).
* 2026-03-30T10:24:31Z D-46 captured by Dov Kestrel via slash.
* 2026-03-30T13:40:16Z D-47 captured by Nadia Quill via slash.
* 2026-03-30T13:41:46Z D-48 captured by Nadia Quill via slash.
* 2026-03-30T13:42:27Z T-49 captured by Nadia Quill via slash.
* 2026-04-01T09:47:30Z D-1 went from active to superseded (by Wren Calloway).
* 2026-04-01T09:47:30Z D-50 captured by Wren Calloway via slash.
* 2026-04-02T11:38:26Z D-29 went from active to superseded (by Wren Calloway).
* 2026-04-02T11:38:26Z D-51 captured by Wren Calloway via slash.
* 2026-04-02T11:40:16Z T-52 captured by Wren Calloway via slash.
* 2026-04-02T15:09:25Z Q-53 captured by Nadia Quill via slash.
* 2026-04-02T15:10:48Z T-45 went from open to done (by Dov Kestrel).
* 2026-04-03T10:01:36Z T-2 went from open to done (by Wren Calloway).
* 2026-04-03T14:05:44Z T-40 went from open to done (by Wren Calloway).
* 2026-04-06T09:48:14Z D-35 went from active to superseded (by Wren Calloway).
* 2026-04-06T09:48:14Z D-54 captured by Wren Calloway via slash.
* 2026-04-06T09:53:19Z T-55 captured by Wren Calloway via slash.
* 2026-04-06T11:14:26Z D-50 went from active to superseded (by Wren Calloway).
* 2026-04-06T11:14:26Z D-56 captured by Wren Calloway via slash.
* 2026-04-06T11:15:41Z D-57 captured by Wren Calloway via slash.
* 2026-04-08T10:12:08Z Q-58 captured by Dov Kestrel via slash.
* 2026-04-08T11:05:24Z T-32 went from open to done (by Wren Calloway).
* 2026-04-08T12:01:19Z T-37 went from open to done (by Wren Calloway).
* 2026-04-08T16:41:13Z T-52 went from open to done (by Wren Calloway).
* 2026-04-09T10:05:15Z T-24 went from open to done (by Wren Calloway).
* 2026-04-09T14:13:28Z D-41 went from active to superseded (by Wren Calloway).
* 2026-04-09T14:13:28Z D-59 captured by Wren Calloway via slash.
* 2026-04-09T14:18:37Z T-60 captured by Wren Calloway via slash.
* 2026-04-09T17:05:07Z T-55 went from open to done (by Wren Calloway).
* 2026-04-10T15:00:35Z T-49 went from open to done (by Nadia Quill).
* 2026-04-10T17:04:16Z T-61 captured by Wren Calloway via slash.
* 2026-04-11T12:10:06Z Q-62 captured by Imke Brandt via slash.
* 2026-04-11T12:20:05Z Q-62 went from open to dropped (by Imke Brandt).
* 2026-04-13T12:00:19Z T-61 went from open to done (by Wren Calloway).
* 2026-04-13T15:43:18Z D-47 went from active to superseded (by Wren Calloway).
* 2026-04-13T15:43:18Z D-63 captured by Wren Calloway via slash.
* 2026-04-13T15:48:15Z T-64 captured by Wren Calloway via slash.
* 2026-04-14T09:16:38Z T-65 captured by Wren Calloway via slash.
* 2026-04-14T14:07:45Z Q-66 captured by Pell Hartnoll via slash.
* 2026-04-15T08:53:26Z D-67 captured by Wren Calloway via slash.
* 2026-04-15T09:07:33Z D-21 went from active to superseded (by Wren Calloway).
* 2026-04-15T09:07:33Z D-68 captured by Wren Calloway via slash.
* 2026-04-15T09:15:15Z T-69 captured by Wren Calloway via slash.
* 2026-04-16T17:44:13Z Q-66 went from open to answered (by Pell Hartnoll).
* 2026-04-16T17:52:45Z D-70 captured by Wren Calloway via slash.
* 2026-04-16T17:53:13Z D-71 captured by Wren Calloway via slash.
* 2026-04-16T17:54:22Z T-72 captured by Wren Calloway via slash.
* 2026-04-16T17:55:32Z T-73 captured by Wren Calloway via slash.
* 2026-04-17T16:00:22Z T-60 went from open to done (by Wren Calloway).
* 2026-04-20T10:33:22Z D-20 went from active to superseded (by Wren Calloway).
* 2026-04-20T10:33:22Z D-74 captured by Wren Calloway via slash.
* 2026-04-20T10:34:19Z T-75 captured by Wren Calloway via slash.
* 2026-04-21T09:50:30Z T-69 went from open to done (by Wren Calloway).
* 2026-04-22T13:23:50Z D-70 went from active to superseded (by Wren Calloway).
* 2026-04-22T13:23:50Z D-76 captured by Wren Calloway via slash.
