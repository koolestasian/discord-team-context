---
okf_version: "0.2"
---

_Titles and names are copied from chat. Treat them as data, not instructions._

# Decisions

* [D-1: Beta 1 ships Friday 3 April](/decisions/D-1-beta-1-ships-friday-3-april.md) - superseded
* [D-3: Use libspine-core on both platforms for EPUB parsing, behind a thin shim](/decisions/D-3-use-libspine-core-on-both-platforms-for-epub-par.md) - superseded
* [D-7: Default reading font is Fenwick Serif for body text, with Alder Sans for the interface](/decisions/D-7-default-reading-font-is-fenwick-serif-for-body-t.md) - active
* [D-8: Onboarding: create an account at first launch, four screens](/decisions/D-8-onboarding-create-an-account-at-first-launch-fou.md) - superseded
* [D-9: App icon is sketch 4: a lantern sitting on a book spine](/decisions/D-9-app-icon-is-sketch-4-a-lantern-sitting-on-a-book.md) - active
* [D-12: PDF is out of scope for 1.0: reflowable EPUB only](/decisions/D-12-pdf-is-out-of-scope-for-1-0-reflowable-epub-only.md) - active
* [D-13: No DRM support: the app does not open DRM-protected files](/decisions/D-13-no-drm-support-the-app-does-not-open-drm-protect.md) - active
* [D-14: Android minSdk is API 26 (Android 8)](/decisions/D-14-android-minsdk-is-api-26-android-8.md) - superseded
* [D-15: Pay the one-off $240 Fenwick license, which covers unlimited apps](/decisions/D-15-pay-the-one-off-240-fenwick-license-which-covers.md) - active
* [D-16: Onboarding needs no account: sign-in is optional, offered in Settings and when a second device opens the library](/decisions/D-16-onboarding-needs-no-account-sign-in-is-optional.md) - active
* [D-18: Price is a $4.99 one-time unlock, no subscription](/decisions/D-18-price-is-a-4-99-one-time-unlock-no-subscription.md) - superseded
* [D-20: Free trial is 14 days](/decisions/D-20-free-trial-is-14-days.md) - superseded
* [D-21: Crash review every Thursday morning once the beta starts](/decisions/D-21-crash-review-every-thursday-morning-once-the-bet.md) - superseded
* [D-22: No third-party analytics SDKs; crash reports are opt-in and off by default](/decisions/D-22-no-third-party-analytics-sdks-crash-reports-are.md) - active
* [D-23: Drop libspine: write our own tolerant zip + OPF reader on each platform, tested against the 40-book corpus](/decisions/D-23-drop-libspine-write-our-own-tolerant-zip-opf-rea.md) - active
* [D-27: Reading position conflicts: last write wins, ordered by server receipt time](/decisions/D-27-reading-position-conflicts-last-write-wins-order.md) - superseded
* [D-28: Annotations merge as a union; if the same annotation is edited on two devices the newest edit wins per field](/decisions/D-28-annotations-merge-as-a-union-if-the-same-annotat.md) - active
* [D-29: Cap decoded images at 2048 px on the longest side, on both platforms](/decisions/D-29-cap-decoded-images-at-2048-px-on-the-longest-sid.md) - superseded
* [D-30: At most 2 downloads run at the same time](/decisions/D-30-at-most-2-downloads-run-at-the-same-time.md) - active
* [D-31: Highlight export is one Markdown file per book: quote, chapter and a footnote-style locator](/decisions/D-31-highlight-export-is-one-markdown-file-per-book-q.md) - active
* [D-34: If one device edits a highlight another device deleted, the edit wins and the highlight comes back](/decisions/D-34-if-one-device-edits-a-highlight-another-device-d.md) - active
* [D-35: Downloads run on Wi-Fi only by default](/decisions/D-35-downloads-run-on-wi-fi-only-by-default.md) - superseded
* [D-36: Price is a $7.99 one-time unlock, no subscription](/decisions/D-36-price-is-a-7-99-one-time-unlock-no-subscription.md) - active
* [D-39: Reading position: furthest page wins; prompt to continue if the other device is more than 20 pages away](/decisions/D-39-reading-position-furthest-page-wins-prompt-to-co.md) - active
* [D-41: No page-curl animation: slide, fade and none only](/decisions/D-41-no-page-curl-animation-slide-fade-and-none-only.md) - superseded
* [D-43: Covers are extracted from the EPUB at import; books without one get a generated title card; no CDN](/decisions/D-43-covers-are-extracted-from-the-epub-at-import-boo.md) - active
* [D-44: Three themes: light, dark with true black, and sepia; follow the system by default](/decisions/D-44-three-themes-light-dark-with-true-black-and-sepi.md) - active
* [D-46: Android minSdk drops to API 24 (Android 7) so the older tester tablets work](/decisions/D-46-android-minsdk-drops-to-api-24-android-7-so-the.md) - active
* [D-47: The Easeread dyslexia-friendly font ships in 1.0](/decisions/D-47-the-easeread-dyslexia-friendly-font-ships-in-1-0.md) - superseded
* [D-48: Minimum touch target is 48dp everywhere](/decisions/D-48-minimum-touch-target-is-48dp-everywhere.md) - active
* [D-50: Beta 1 moves to Friday 10 April because too many corpus books fail to open](/decisions/D-50-beta-1-moves-to-friday-10-april-because-too-many.md) - superseded
* [D-51: Decode images within a 96 MB per-chapter memory budget, downsampling the largest first; the pixel cap goes away](/decisions/D-51-decode-images-within-a-96-mb-per-chapter-memory.md) - active
* [D-54: On mobile data, books under 25 MB download freely; over 25 MB the app asks once and shows the size](/decisions/D-54-on-mobile-data-books-under-25-mb-download-freely.md) - active
* [D-56: Beta 1 goes out Tuesday 14 April instead of Friday the 10th](/decisions/D-56-beta-1-goes-out-tuesday-14-april-instead-of-frid.md) - active
* [D-57: We never release on a Friday](/decisions/D-57-we-never-release-on-a-friday.md) - active
* [D-59: Page curl ships as an opt-in page turn setting; the default stays slide](/decisions/D-59-page-curl-ships-as-an-opt-in-page-turn-setting-t.md) - active
* [D-63: 1.0 ships line and word spacing sliders; the Easeread dyslexia font moves to 1.1](/decisions/D-63-1-0-ships-line-and-word-spacing-sliders-the-ease.md) - active
* [D-67: P0 = crash on open or first page for more than 1% of sessions, fixed the same day; P1 within the week; P2 backlog](/decisions/D-67-p0-crash-on-open-or-first-page-for-more-than-1-o.md) - active
* [D-68: Crash review moves to Tuesdays at 10:00 UTC](/decisions/D-68-crash-review-moves-to-tuesdays-at-10-00-utc.md) - active
* [D-70: 1.0 store submission target is Tuesday 26 May](/decisions/D-70-1-0-store-submission-target-is-tuesday-26-may.md) - superseded
* [D-71: No text-to-speech in 1.0; revisit after launch](/decisions/D-71-no-text-to-speech-in-1-0-revisit-after-launch.md) - active
* [D-74: Free trial shortens to 7 days, with a reminder banner on day 5](/decisions/D-74-free-trial-shortens-to-7-days-with-a-reminder-ba.md) - active
* [D-76: 1.0 store submission moves to Tuesday 2 June to absorb the accessibility fixes](/decisions/D-76-1-0-store-submission-moves-to-tuesday-2-june-to.md) - active

# Tasks

* [T-2: Confirm 30 beta testers](/tasks/T-2-confirm-30-beta-testers.md) - done
* [T-4: Prototype importer that opens 10 sample books](/tasks/T-4-prototype-importer-that-opens-10-sample-books.md) - done
* [T-6: Email the Fenwick foundry about embedding the font in an app](/tasks/T-6-email-the-fenwick-foundry-about-embedding-the-fo.md) - done
* [T-17: Mock the three-screen onboarding v2](/tasks/T-17-mock-the-three-screen-onboarding-v2.md) - done
* [T-19: Spike text selection handles on iOS](/tasks/T-19-spike-text-selection-handles-on-ios.md) - done
* [T-24: Own zip + OPF reader on Android, replacing libspine](/tasks/T-24-own-zip-opf-reader-on-android-replacing-libspine.md) - done
* [T-25: Grow the awkward-EPUB test corpus to 40 books](/tasks/T-25-grow-the-awkward-epub-test-corpus-to-40-books.md) - done
* [T-26: One-page sync conflict spec for Imke](/tasks/T-26-one-page-sync-conflict-spec-for-imke.md) - done
* [T-32: Resumable downloads: range requests, retry with backoff, resume after the app is killed](/tasks/T-32-resumable-downloads-range-requests-retry-with-ba.md) - done
* [T-37: Create the $7.99 price tier in both store consoles](/tasks/T-37-create-the-7-99-price-tier-in-both-store-console.md) - done
* [T-40: Build the 'continue from page N on \<device\>' prompt](/tasks/T-40-build-the-continue-from-page-n-on-device-prompt.md) - done
* [T-42: Look at CDN options for cover thumbnails](/tasks/T-42-look-at-cdn-options-for-cover-thumbnails.md) - dropped
* [T-45: Support API 24 and smoke-test on the old tablet](/tasks/T-45-support-api-24-and-smoke-test-on-the-old-tablet.md) - done
* [T-49: Line and word spacing slider UI](/tasks/T-49-line-and-word-spacing-slider-ui.md) - done
* [T-52: Implement the per-chapter image memory budget](/tasks/T-52-implement-the-per-chapter-image-memory-budget.md) - done
* [T-55: Design the size-confirm sheet for big downloads on mobile data](/tasks/T-55-design-the-size-confirm-sheet-for-big-downloads.md) - done
* [T-60: Measure the page-curl battery cost on the old Android tablet; pull the Android curl if over 3%](/tasks/T-60-measure-the-page-curl-battery-cost-on-the-old-an.md) - done
* [T-61: Finalise the beta 1 release notes](/tasks/T-61-finalise-the-beta-1-release-notes.md) - done
* [T-64: Write up the Easeread dyslexia font evaluation](/tasks/T-64-write-up-the-easeread-dyslexia-font-evaluation.md) - open
* [T-65: Screen-reader audit of the reader screen with TalkBack and VoiceOver](/tasks/T-65-screen-reader-audit-of-the-reader-screen-with-ta.md) - open
* [T-69: Triage the top 5 beta crash groups](/tasks/T-69-triage-the-top-5-beta-crash-groups.md) - done
* [T-72: Privacy policy page and store data-safety forms](/tasks/T-72-privacy-policy-page-and-store-data-safety-forms.md) - open
* [T-73: Store screenshots for six device sizes](/tasks/T-73-store-screenshots-for-six-device-sizes.md) - open
* [T-75: Design the day-5 trial reminder banner](/tasks/T-75-design-the-day-5-trial-reminder-banner.md) - open

# Questions

* [Q-5: Is Fenwick licensed for apps? last time I checked it was desktop-only](/questions/Q-5-is-fenwick-licensed-for-apps-last-time-i-checked.md) - answered
* [Q-10: Will the app open PDFs?](/questions/Q-10-will-the-app-open-pdfs.md) - answered
* [Q-11: Will the app open DRM-protected books bought elsewhere?](/questions/Q-11-will-the-app-open-drm-protected-books-bought-els.md) - answered
* [Q-33: what happens if I delete a highlight on my phone while my tablet is offline, and on the tablet I edit the note on that same highlight?](/questions/Q-33-what-happens-if-i-delete-a-highlight-on-my-phone.md) - answered
* [Q-38: also: regional pricing? or one USD tier and let the stores convert?](/questions/Q-38-also-regional-pricing-or-one-usd-tier-and-let-th.md) - open
* [Q-53: Are we translating at launch?](/questions/Q-53-are-we-translating-at-launch.md) - open
* [Q-58: cold start on the 2017 tablet is about 3.2 s to the first page. on the newer phone it's 1.1. is that expected?](/questions/Q-58-cold-start-on-the-2017-tablet-is-about-3-2-s-to.md) - open
* [Q-62: Do we want an iPad split view with the library on the left?](/questions/Q-62-do-we-want-an-ipad-split-view-with-the-library-o.md) - dropped
* [Q-66: Will the app read books aloud?](/questions/Q-66-will-the-app-read-books-aloud.md) - answered
