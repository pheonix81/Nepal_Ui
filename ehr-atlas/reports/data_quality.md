# EHR Atlas — data quality report

Generated 2026-11-05 15:10 UTC by `ehr-atlas report`. Regenerate after every build; do not hand-edit.

## Sources

| source | path | upstream date | downloaded_at | bytes | sha256 |
|---|---|---|---|---|---|
| cms_hospitals | data/raw/cms_hospitals/2026-11-05_Hospital_General_Information.csv | 2026-07-22 | 2026-11-05 15:10 UTC | 1450767 | f874f09fef89 |
| cms_pi | data/raw/cms_pi/2026-11-05_Promoting_Interoperability-Hospital.csv | 2026-07-22 | 2026-11-05 15:10 UTC | 647313 | 77289afb3acb |
| onc_linkage | data/raw/onc_linkage/2026-11-05_hospital-promoting-interoperability-chpl-linkage.csv |  | 2026-11-05 15:10 UTC | 18855510 | 4f597ed929a9 |

CHPL cache: 0 CEHRT IDs on disk, 0 resolved OK, 0 not found / error / no products.

## Table sizes

| table | rows |
|---|---|
| hospitals | 5482 |
| hospital_ehr | 67383 |
| hospital_vendor_year | 8108 |
| hospital_current_vendor (with vendor) | 4227 |
| stg_cms_hospitals | 5419 |
| stg_onc_linkage | 68446 |
| stg_cms_pi | 4626 |
| stg_chpl_cehrt | 0 |

## 1. Match rate: CMS hospitals → vendor, by year

Denominator is every CCN in CMS Hospital General Information. Psychiatric, VA, DoD and most children's hospitals are not in the Medicare Promoting Interoperability program, so they are expected to be unmatched; the second table breaks this out.

| year | CMS hospitals | with any EHR row | with primary vendor | match % | CCNs not in CMS list |
|---|---|---|---|---|---|
| 2023 | 5419 | 4000 | 4000 | 73.8 | 46 |
| 2024 | 5419 | 4041 | 4040 | 74.6 | 21 |

### By hospital type (latest year)

| hospital type | hospitals | with vendor | match % |
|---|---|---|---|
| Acute Care Hospitals | 3107 | 2940 | 94.6 |
| Critical Access Hospitals | 1382 | 1239 | 89.7 |
| Psychiatric | 624 | 0 | 0 |
| Acute Care - Veterans Administration | 132 | 0 | 0 |
| Childrens | 94 | 2 | 2.1 |
| Rural Emergency Hospital | 43 | 0 | 0 |
| Acute Care - Department of Defense | 32 | 0 | 0 |
| Long-term | 5 | 0 | 0 |

### Acute-care + critical-access hospitals only

| year | ACH + CAH | with primary vendor | match % |
|---|---|---|---|
| 2023 | 4489 | 4000 | 89.1 |
| 2024 | 4489 | 4040 | 90 |

Hospitals present in the PI files with no CEHRT ID reported (attested nothing that year): | year | hospitals |
|---|---|
| 2023 | 547 |
| 2024 | 569 |


## 2. Unresolved CEHRT IDs

CEHRT IDs attested by a hospital that neither ONC's linkage nor the CHPL cache expands to products. Fix by running `ehr-atlas chpl` with `CHPL_API_KEY` set.

| cehrt_id | source | years | hospitals | CHPL status | CHPL id from ONC |
|---|---|---|---|---|---|
| 0015CSP20HYW41Z | onc_linkage | 2024,2023 | 4 | not in CHPL cache | 15.04.04.2806.Open.02.02.1.221222 |
| 0015CCKOR5UQ3Q3 | cms_pi | 2024 | 2 | not in CHPL cache |  |
| 0015CK8KUDD3U5B | cms_pi | 2024 | 2 | not in CHPL cache |  |
| 0015CGX7B2D4KGU | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015CCJ4SAG3GA6 | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015CEM5ZONMV82 | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015CFG3Q10HY2B | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015CH3UXCODT1U | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015CFG3Q10HYZV | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015CFU9FK26P73 | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015C7PD0YBKOUE | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015CDQ4NTOCME6 | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015CQKE2ES6CY3 | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015C0UEC9PUC4E | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015CVP41SORH2B | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015CQ7531ZYB8L | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015CKD5SWZ3QX3 | onc_linkage | 2024 | 1 | not in CHPL cache | 15.05.05.2538.WLST.01.01.1.180703 |
| 0015COUVC9DUC4E | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015CFG2CWR0503 | onc_linkage | 2023 | 1 | not in CHPL cache | 15.05.05.2538.WLST.01.01.1.180703 |
| 0015CUQ8KZWI8W7 | cms_pi | 2024 | 1 | not in CHPL cache |  |
| 0015CXZ21PB054A | cms_pi | 2024 | 1 | not in CHPL cache |  |

Hospital-years affected: 1

## 3. Hospitals with multiple vendors

A CEHRT ID bundles several certified modules, often from more than one developer (e.g. Epic plus an Oracle Health eCQM module). Only *core-EHR-tier* vendors matter for the primary-vendor question.

| year | hospital-years | >1 vendor (any tier) | >1 core-EHR vendor | unbroken ties |
|---|---|---|---|---|
| 2023 | 4046 | 2075 | 443 | 7 |
| 2024 | 4062 | 2061 | 367 | 6 |

### How the primary vendor was decided

| rule | hospital-years |
|---|---|
| core_product | 3987 |
| single_vendor | 3971 |
| module_count | 94 |
| vendor_tier | 42 |
| alphabetical_tie | 13 |
| unresolved | 1 |

### Most common core-vendor combinations

| core vendors present | chosen | hospital-years |
|---|---|---|
| Altera Digital Health\|MEDITECH | MEDITECH | 276 |
| Epic\|Oracle Health | Epic | 169 |
| MEDITECH\|TruBridge | MEDITECH | 54 |
| Epic\|Oracle Health | Oracle Health | 49 |
| Altera Digital Health\|Veradigm | Altera Digital Health | 33 |
| MEDITECH\|NextGen Healthcare | MEDITECH | 31 |
| MEDITECH\|Veradigm | MEDITECH | 26 |
| Oracle Health\|Veradigm | Oracle Health | 23 |
| Altera Digital Health\|Oracle Health | Oracle Health | 17 |
| MEDHOST\|MEDITECH | MEDITECH | 17 |
| Altera Digital Health\|Oracle Health\|Veradigm | Altera Digital Health | 16 |
| Epic\|MEDITECH | Epic | 14 |
| Altera Digital Health\|Epic | Epic | 14 |
| Epic\|Veradigm | Epic | 10 |
| Epic\|MEDITECH | MEDITECH | 9 |

### Vendor changes between years

| from | to | year from | year to | hospitals |
|---|---|---|---|---|
| Oracle Health | Epic | 2023 | 2024 | 28 |
| MEDITECH | Epic | 2023 | 2024 | 19 |
| TruBridge | Oracle Health | 2023 | 2024 | 8 |
| Altera Digital Health | Epic | 2023 | 2024 | 7 |
| Epic | MEDITECH | 2023 | 2024 | 6 |
| TruBridge | Epic | 2023 | 2024 | 6 |
| MEDHOST | Oracle Health | 2023 | 2024 | 5 |
| TruBridge | MEDITECH | 2023 | 2024 | 3 |
| athenahealth | Oracle Health | 2023 | 2024 | 2 |
| MEDITECH | Oracle Health | 2023 | 2024 | 2 |
| NextGen Healthcare | Oracle Health | 2023 | 2024 | 1 |
| One Medical | TruBridge | 2023 | 2024 | 1 |
| Oracle Health | TruBridge | 2023 | 2024 | 1 |
| MEDITECH | athenahealth | 2023 | 2024 | 1 |
| MEDITECH | Altera Digital Health | 2023 | 2024 | 1 |
| Oracle Health | MEDITECH | 2023 | 2024 | 1 |
| MEDHOST | Epic | 2023 | 2024 | 1 |
| VIPA Health Solutions | TruBridge | 2023 | 2024 | 1 |
| Dynamic Health IT | Epic | 2023 | 2024 | 1 |
| Altera Digital Health | Oracle Health | 2023 | 2024 | 1 |

Hospitals whose primary vendor changed year over year: 97

## 4. Vendor share by hospital count

Primary vendor per hospital, latest year available for that hospital (`hospital_current_vendor`).

| vendor | hospitals | share % | acute care | critical access | other/unknown |
|---|---|---|---|---|---|
| Epic | 1969 | 46.6 | 1494 | 464 | 11 |
| Oracle Health | 901 | 21.3 | 592 | 295 | 14 |
| MEDITECH | 674 | 15.9 | 491 | 176 | 7 |
| TruBridge | 332 | 7.9 | 117 | 209 | 6 |
| Altera Digital Health | 119 | 2.8 | 101 | 17 | 1 |
| MEDHOST | 116 | 2.7 | 90 | 22 | 4 |
| athenahealth | 55 | 1.3 | 9 | 43 | 3 |
| Indian Health Service (RPMS) | 19 | 0.4 | 15 | 4 | 0 |
| Infomedika | 16 | 0.4 | 15 | 0 | 1 |
| Azalea Health | 12 | 0.3 | 5 | 6 | 1 |
| Dynamic Health IT | 4 | 0.1 | 4 | 0 | 0 |
| Sabiamed | 3 | 0.1 | 3 | 0 | 0 |
| Empower Systems | 1 | 0 | 1 | 0 | 0 |
| Greenway Health | 1 | 0 | 0 | 1 | 0 |
| CompuGroup Medical | 1 | 0 | 0 | 1 | 0 |
| Tenzing Medical | 1 | 0 | 1 | 0 | 0 |
| Amrita Ventures | 1 | 0 | 0 | 1 | 0 |
| iCare | 1 | 0 | 1 | 0 | 0 |
| Clinisys | 1 | 0 | 1 | 0 | 0 |

### By year (all hospital-years, not just latest)

| vendor | 2023 | 2024 |
|---|---|---|
| Epic | 1804 | 1899 |
| Oracle Health | 892 | 852 |
| MEDITECH | 694 | 679 |
| TruBridge | 315 | 301 |
| MEDHOST | 119 | 113 |
| Altera Digital Health | 113 | 113 |
| athenahealth | 50 | 51 |
| Indian Health Service (RPMS) | 19 | 18 |
| Infomedika | 16 | 13 |
| Azalea Health | 8 | 11 |
| Dynamic Health IT | 4 | 4 |
| Sabiamed | 3 | 0 |
| CompuGroup Medical | 1 | 1 |
| Clinisys | 1 | 1 |
| One Medical | 1 | 0 |

### By state (top vendor per state, latest year)

| state | hospitals | top vendor | top vendor % |
|---|---|---|---|
| AK | 20 | Oracle Health | 45 |
| AL | 82 | Oracle Health | 27 |
| AR | 70 | Epic | 30 |
| AZ | 73 | Oracle Health | 53 |
| CA | 288 | Epic | 45 |
| CO | 82 | Epic | 49 |
| CT | 23 | Epic | 70 |
| DC | 6 | Oracle Health | 67 |
| DE | 6 | Epic | 67 |
| FL | 175 | MEDITECH | 31 |
| GA | 120 | Epic | 48 |
| HI | 18 | Epic | 56 |
| IA | 113 | Epic | 45 |
| ID | 35 | Epic | 31 |
| IL | 161 | Epic | 58 |
| IN | 97 | Epic | 45 |
| KS | 121 | Oracle Health | 36 |
| KY | 87 | MEDITECH | 36 |
| LA | 104 | Epic | 38 |
| MA | 55 | Epic | 51 |
| MD | 43 | Epic | 53 |
| ME | 31 | Epic | 39 |
| MI | 114 | Epic | 49 |
| MN | 122 | Epic | 84 |
| MO | 97 | Epic | 41 |
| MS | 79 | Epic | 49 |
| MT | 53 | Oracle Health | 36 |
| NC | 98 | Epic | 68 |
| ND | 42 | Epic | 57 |
| NE | 81 | Oracle Health | 36 |
| NH | 26 | Epic | 35 |
| NJ | 60 | Epic | 70 |
| NM | 37 | Epic | 41 |
| NV | 29 | Oracle Health | 48 |
| NY | 143 | Epic | 52 |
| OH | 148 | Epic | 72 |
| OK | 107 | Epic | 47 |
| OR | 57 | Epic | 77 |
| PA | 142 | Epic | 52 |
| PR | 43 | MEDITECH | 53 |
| RI | 10 | Epic | 50 |
| SC | 53 | Epic | 74 |
| SD | 57 | Epic | 61 |
| TN | 90 | Oracle Health | 41 |
| TX | 325 | Epic | 36 |
| UT | 44 | Oracle Health | 59 |
| VA | 78 | Epic | 73 |
| VT | 14 | Oracle Health | 36 |
| WA | 79 | Epic | 71 |
| WI | 119 | Epic | 82 |
| WV | 45 | Epic | 40 |
| WY | 25 | Oracle Health | 48 |

## 5. Spot checks

Well-known hospitals with an externally known vendor (see `config/spot_checks.csv`). `all vendors` shows every developer under the CEHRT ID.

| ccn | name | expected | 2023 | 2024 | result | core vendors | rule | notes |
|---|---|---|---|---|---|---|---|---|
| 030002 | Banner - University Medical Center Phoenix | Oracle Health | Oracle Health | Oracle Health | PASS | Oracle Health | single_vendor | Banner is a long-time Cerner shop |
| 050625 | Cedars-Sinai Medical Center | Epic | Epic | Epic | PASS | Epic | core_product |  |
| 140281 | Northwestern Memorial Hospital | Epic | Epic | Epic | PASS | Epic | single_vendor |  |
| 210009 | Johns Hopkins Hospital | Epic | Epic | Epic | PASS | Epic | single_vendor |  |
| 220071 | Massachusetts General Hospital | Epic | Epic | Epic | PASS | Epic | core_product |  |
| 240010 | Mayo Clinic Hospital Rochester | Epic | Epic | Epic | PASS | Epic | core_product |  |
| 330101 | NewYork-Presbyterian Hospital | Epic | Epic | Epic | PASS | Epic\|Oracle Health | core_product |  |
| 330195 | Long Island Jewish Medical Center | Altera Digital Health | Altera Digital Health | Altera Digital Health | PASS | Altera Digital Health\|Oracle Health\|Veradigm | core_product | Northwell on Allscripts/Altera Sunrise through 2024; Epic go-live 2025 |
| 360180 | Cleveland Clinic | Epic | Epic | Epic | PASS | Epic | single_vendor |  |
| 390164 | UPMC Presbyterian Shadyside | Oracle Health | Oracle Health | Oracle Health | PASS | Epic\|Oracle Health | core_product | UPMC inpatient is Cerner; ambulatory is Epic - expect both vendors to appear |
| 440039 | Vanderbilt University Medical Center | Epic | Epic | Epic | PASS | Epic\|Oracle Health | module_count |  |
| 450358 | Houston Methodist Hospital | Epic | Epic | Epic | PASS | Epic | core_product |  |
| 450647 | Medical City Dallas Hospital | MEDITECH | MEDITECH | MEDITECH | PASS | Altera Digital Health\|MEDITECH | core_product | HCA facility; HCA runs MEDITECH with its own hCare add-ons |
| 460010 | Intermountain Medical Center | Oracle Health | Oracle Health | Oracle Health | PASS | Oracle Health | core_product | Cerner through 2024; Intermountain went live on Epic in 2025 |

### Vendor overrides

Hand-entered corrections from `config/vendor_overrides.csv`, for hospitals that switched vendor after their latest attestation or are outside the PI program. These win over the attested vendor (`rule = override`).

| ccn | name | state | attested | year | override | as of | source | notes |
|---|---|---|---|---|---|---|---|---|
| 161351 | AVERA HOLY FAMILY HOSPITAL | IA | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 161321 | AVERA MERRILL PIONEER HOSPITAL | IA | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 170105 | MCPHERSON HOSPITAL | KS | TruBridge | 2024 | MEDITECH | 2026-10-04 | user feedback | Migrated from TruBridge to MEDITECH in 2026; its FHIR endpoint is already MEDITECH |
| 190298 | CHRISTUS CENTRAL LOUISIANA SURGICAL HOSPITAL | LA | Altera Digital Health | 2024 | Epic | 2026-10-04 | user feedback | CHRISTUS now 100% Epic |
| 191312 | CHRISTUS COUSHATTA HEALTH CARE CENTER | LA | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | CHRISTUS now 100% Epic |
| 190201 | CHRISTUS OCHSNER LAKE AREA HOSPITAL | LA | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | CHRISTUS now 100% Epic |
| 190027 | CHRISTUS OCHSNER ST PATRICK HOSPITAL | LA | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | CHRISTUS now 100% Epic |
| 190041 | CHRISTUS SHREVEPORT-BOSSIER HEALTH SYSTEM | LA | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | CHRISTUS now 100% Epic |
| 190019 | CHRISTUS ST FRANCES CABRINI HOSPITAL | LA | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | CHRISTUS now 100% Epic |
| 201310 | BRIDGTON HOSPITAL | ME | Oracle Health | 2024 | Epic | 2026-10-04 | user feedback | Central Maine Healthcare moved from Oracle Health to Epic |
| 200024 | CENTRAL MAINE MEDICAL CENTER | ME | Oracle Health | 2024 | Epic | 2026-10-04 | user feedback | Central Maine Healthcare moved from Oracle Health to Epic |
| 201306 | RUMFORD  HOSPITAL | ME | Oracle Health | 2024 | Epic | 2026-10-04 | user feedback | Central Maine Healthcare moved from Oracle Health to Epic |
| 241343 | AVERA GRANITE FALLS | MN | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 241359 | AVERA MARSHALL REGIONAL MEDICAL CTR | MN | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 241348 | AVERA TYLER HOSPITAL | MN | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 250167 | METHODIST HEALTHCARE - OLIVE BRANCH HOSPITAL | MS | Oracle Health | 2024 | Epic | 2026-10-04 | user feedback | Methodist Le Bonheur system-wide Epic go-live |
| 281331 | AVERA CREIGHTON HOSPITAL | NE | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 281329 | AVERA ST ANTHONY'S HOSPITAL | NE | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 320004 | CHRISTUS SOUTHERN NEW MEXICO | NM | Oracle Health | 2024 | Epic | 2026-10-04 | user feedback | CHRISTUS now 100% Epic (AHRQ 2023 still lists it under Gerald Champion) |
| 431332 | AVERA DE SMET MEMORIAL HOSPITAL - CAH | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 431331 | AVERA DELLS AREA HOSPITAL  - CAH | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 431310 | AVERA FLANDREAU HOSPITAL - CAH | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 431338 | AVERA GREGORY HOSPITAL | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 431337 | AVERA HAND COUNTY MEMORIAL HOSPITAL AND CLINIC | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 430095 | AVERA HEART HOSPITAL OF SOUTH DAKOTA | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 430016 | AVERA MCKENNAN HOSPITAL & UNIVERSITY HEALTH CENTER | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 431302 | AVERA MISSOURI RIVER HEALTH CENTER | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 431340 | AVERA QUEEN OF PEACE | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 430012 | AVERA SACRED HEART HOSPITAL | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 431330 | AVERA ST BENEDICT HEALTH CENTER - CAH | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 430014 | AVERA ST LUKES | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 430015 | AVERA ST MARY'S HOSPITAL | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 431324 | AVERA WESKOTA MEMORIAL MEDICAL CENTER - CAH | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 431326 | MILBANK AREA HOSPITAL/AVERA HEALTH | SD | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | Avera Health Epic migration in 2026 |
| 440049 | METHODIST HOSPITALS OF MEMPHIS | TN | Oracle Health | 2024 | Epic | 2026-10-04 | user feedback | Methodist Le Bonheur went live on Epic at all sites in 2024 or 2025; clinicians already attest Epic |
| 443302 | ST JUDE CHILDRENS RESEARCH HOSPITAL | TN |  |  | Epic | 2026-10-04 | user feedback | Children's hospital outside the PI program; its FHIR endpoint is Epic |
| 453315 | CHILDREN'S HOSPITAL OF SAN ANTONIO | TX |  |  | Epic | 2026-10-04 | user feedback | CHRISTUS-owned children's hospital outside the PI program; CHRISTUS now 100% Epic |
| 450573 | CHRISTUS JASPER MEMORIAL HOSPITAL | TX | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | CHRISTUS now 100% Epic |
| 450237 | CHRISTUS SANTA ROSA MEDICAL CENTER | TX | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | CHRISTUS now 100% Epic |
| 450034 | CHRISTUS SOUTHEAST TEXAS- ST ELIZABETH | TX | MEDITECH | 2024 | Epic | 2026-10-04 | user feedback | CHRISTUS now 100% Epic |
| 450068 | MEMORIAL HERMANN - TEXAS MEDICAL CENTER | TX | Oracle Health | 2024 | Epic | 2026-10-04 | user feedback | Memorial Hermann 100% Epic; clinicians already attest Epic |
| 450184 | MEMORIAL HERMANN HOSPITAL SYSTEM | TX | Oracle Health | 2024 | Epic | 2026-10-04 | user feedback | Memorial Hermann 100% Epic; clinicians already attest Epic |
| 450847 | MEMORIAL HERMANN KATY HOSPITAL | TX | Oracle Health | 2024 | Epic | 2026-10-04 | user feedback | Memorial Hermann 100% Epic |
| 450610 | MEMORIAL HERMANN MEMORIAL CITY HOSPITAL | TX | Oracle Health | 2024 | Epic | 2026-10-04 | user feedback | Memorial Hermann 100% Epic |
| 450684 | MEMORIAL HERMANN NORTHEAST HOSPITAL | TX | Oracle Health | 2024 | Epic | 2026-10-04 | user feedback | Memorial Hermann 100% Epic |
| 450848 | MEMORIAL HERMANN SUGAR LAND HOSPITAL | TX | Oracle Health | 2024 | Epic | 2026-10-04 | user feedback | Memorial Hermann 100% Epic |

## 6. Things that look wrong or need review

### Developer names not in `config/vendor_map.csv`

These fall through with `vendor = developer_name_raw`, `vendor_tier = unmapped`. Add them to the map.

_none_

### Primary vendor is an ancillary-tier developer

No core-EHR vendor appeared under the CEHRT ID; the assignment is a best guess and probably wrong.

| year | primary_vendor | hospitals | example |
|---|---|---|---|
| 2023 | Dynamic Health IT | 4 | BETH ISRAEL DEACONESS MEDICAL CENTER |
| 2024 | Dynamic Health IT | 4 | MEMORIAL HOSPITAL |
| 2023 | VIPA Health Solutions | 1 | WIREGRASS MEDICAL CENTER |
| 2023 | Amrita Ventures | 1 | RIO GRANDE HOSPITAL |
| 2023 | Tenzing Medical | 1 | OROVILLE HOSPITAL |
| 2024 | iCare | 1 | SURGERY SPECIALTY HOSPITALS OF AMERICA SE HOUSTON |
| 2023 | One Medical | 1 | HOSPITAL DISTRICT #1 OF RICE COUNTY |
| 2024 | Clinisys | 1 | HOLY NAME MEDICAL CENTER |
| 2023 | iCare | 1 | SURGERY SPECIALTY HOSPITALS OF AMERICA SE HOUSTON |
| 2023 | Empower Systems | 1 | NEW YORK COMMUNITY HOSPITAL OF BROOKLYN, INC. |
| 2024 | Empower Systems | 1 | NEW YORK COMMUNITY HOSPITAL OF BROOKLYN, INC. |
| 2023 | Clinisys | 1 | HOLY NAME MEDICAL CENTER |
| 2024 | Tenzing Medical | 1 | OROVILLE HOSPITAL |
| 2024 | Amrita Ventures | 1 | RIO GRANDE HOSPITAL |

### Unbroken ties

| ccn | name | year | chosen | core_vendors | all_vendors |
|---|---|---|---|---|---|
| 050103 | ADVENTIST HEALTH WHITE MEMORIAL | 2023 | MEDITECH | MEDITECH\|Oracle Health | MEDITECH\|Medisolv\|Oracle Health\|Secure Exchange Solutions |
| 060006 | MONTROSE REGIONAL HEALTH | 2023 | MEDITECH | MEDITECH\|eClinicalWorks | MEDITECH\|eClinicalWorks |
| 061301 | RIO GRANDE HOSPITAL | 2023 | Amrita Ventures |  | Amrita Ventures\|Darena Health |
| 181327 | MARSHALL COUNTY HOSPITAL | 2023 | CompuGroup Medical | CompuGroup Medical\|TruBridge | CompuGroup Medical\|Secure Exchange Solutions\|TruBridge |
| 270003 | ST PETERS HEALTH | 2023 | Epic | Epic\|MEDITECH | Epic\|MEDITECH\|MaxMD\|Medisolv |
| 310008 | HOLY NAME MEDICAL CENTER | 2023 | Clinisys |  | Clinisys\|DrFirst\|Dynamic Health IT\|Holy Name Medical Center (internal)\|Nuance\|Premier\|Syntellis |
| 440102 | LINCOLN MEDICAL CENTER | 2023 | TruBridge | TruBridge\|athenahealth | TruBridge\|athenahealth |
| 061301 | RIO GRANDE HOSPITAL | 2024 | Amrita Ventures |  | Amrita Ventures\|Darena Health |
| 250081 | ANDERSON REGIONAL MEDICAL CENTER SOUTH CAMPUS | 2024 | Epic | Epic\|MEDITECH | Epic\|MEDITECH |
| 250104 | ANDERSON REGIONAL MEDICAL CENTER | 2024 | Epic | Epic\|MEDITECH | Epic\|MEDITECH |
| 310008 | HOLY NAME MEDICAL CENTER | 2024 | Clinisys |  | Clinisys\|DrFirst\|Dynamic Health IT\|Holy Name Medical Center (internal)\|Nuance\|Premier\|Syntellis |
| 370215 | OKLAHOMA HEART HOSPITAL, LLC | 2024 | Epic | Epic\|Oracle Health | Epic\|Oracle Health |
| 370234 | OKLAHOMA HEART HOSPITAL SOUTH, LLC | 2024 | Epic | Epic\|Oracle Health | Epic\|Oracle Health |

### Core-EHR vendor chosen without a core product row

Chosen on tier/module count only: the vendor's core inpatient product was not among the certified modules (common for MEDITECH sites that list only interfaces). Usually right, worth knowing.

| year | primary_vendor | hospitals |
|---|---|---|
| 2023 | Epic | 38 |
| 2024 | Epic | 15 |
| 2023 | MEDITECH | 13 |
| 2024 | MEDITECH | 9 |
| 2023 | MEDHOST | 8 |
| 2024 | MEDHOST | 5 |
| 2023 | Dynamic Health IT | 4 |
| 2024 | Dynamic Health IT | 4 |
| 2024 | iCare | 1 |
| 2024 | Tenzing Medical | 1 |
| 2024 | Amrita Ventures | 1 |
| 2024 | Clinisys | 1 |
| 2023 | Tenzing Medical | 1 |
| 2023 | iCare | 1 |
| 2023 | Amrita Ventures | 1 |

### CCNs in EHR data but not in CMS Hospital General Information

Kept in `hospitals` with `in_cms_hgi = FALSE` (no type/ownership). Typically closed, merged, or renumbered since the CMS snapshot.

| ccn | name | state | years | vendor |
|---|---|---|---|---|
| 010008 | CRENSHAW COMMUNITY HOSPITAL | AL | 2023,2024 | TruBridge |
| 010018 | CALLAHAN EYE HOSPITAL | AL | 2024,2023 | MEDITECH |
| 010059 | LAWRENCE MEDICAL CENTER | AL | 2024,2023 | MEDHOST |
| 010102 | J PAUL JONES HOSPITAL | AL | 2023,2024 | athenahealth |
| 010120 | MONROE COUNTY HOSPITAL | AL | 2024,2023 | TruBridge |
| 010174 | THOMASVILLE REGIONAL MEDICAL CENTER LLC | AL |  |  |
| 010175 | EAMC-LANIER | AL | 2023 | Oracle Health |
| 030131 | O.A.S.I.S. HOSPITAL | AZ | 2023 | MEDITECH |
| 040015 | MENA REGIONAL HEALTH SYSTEM | AR | 2023 | TruBridge |
| 040067 | MAGNOLIA HOSPITAL | AR | 2023,2024 | TruBridge |
| 050125 | REGIONAL MEDICAL CENTER OF SAN JOSE | CA | 2023,2024 | MEDITECH |
| 050589 | UCI HEALTH - PLACENTIA LINDA | CA | 2024,2023 | Oracle Health |
| 070012 | ROCKVILLE GENERAL HOSPITAL | CT |  |  |
| 100047 | SHOREPOINT HEALTH PUNTA GORDA | FL | 2023,2024 | Oracle Health |
| 100092 | ORLANDO HEALTH ROCKLEDGE HOSPITAL | FL | 2024,2023 | MEDITECH |
| 110153 | HOUSTON HEALTHCARE | GA |  |  |
| 110780 | BLUE RIDGE MEDICAL CENTER | GA |  |  |
| 110781 | TAYLOR REGIONAL HOSPITAL | GA |  |  |
| 131308 | CASCADE MEDICAL CENTER | ID | 2023 | athenahealth |
| 140310 | RUSH SPECIALTY HOSPITAL, LLC | IL |  |  |
| 161300 | MERCYONE PRIMGHAR MEDICAL CENTER | IA | 2023 | Oracle Health |
| 170110 | BOB WILSON MEMORIAL GRANT COUNTY HOSPITAL | KS | 2023 | Epic |
| 180139 | KENTUCKY RIVER MEDICAL CENTER | KY | 2023 | MEDHOST |
| 181332 | FLEMING COUNTY HOSPITAL | KY | 2024,2023 | MEDHOST |
| 200041 | NORTHERN LIGHT INLAND HOSPITAL | ME | 2023,2024 | Oracle Health |
| 200052 | NORTHERN MAINE MEDICAL CENTER | ME | 2023 | Oracle Health |
| 230013 | PONTIAC GENERAL HOSPITAL | MI |  |  |
| 240052 | LAKE REGION HEALTHCARE CORPORATION | MN | 2023,2024 | Epic |
| 251338 | SHARKEY ISSAQUENA COMMUNITY HOSPITAL | MS |  |  |
| 260050 | MOSAIC MEDICAL CENTER - MARYVILLE | MO | 2023 | Epic |
| 260176 | ST LUKE'S DES PERES HOSPITAL | MO | 2023,2024 | Oracle Health |
| 260210 | SOUTH CITY HOSPITAL | MO |  |  |
| 290061 | NORTHERN NEVADA SIERRA MEDICAL CENTER | NV | 2023 | Oracle Health |
| 330060 | CARTHAGE AREA HOSPITAL, INC | NY |  |  |
| 330144 | IRA DAVENPORT MEMORIAL HOSPITAL | NY |  |  |
| 330211 | CLAXTON-HEPBURN MEDICAL CENTER | NY |  |  |
| 331307 | CLIFTON-FINE HOSPITAL | NY | 2023 | TruBridge |
| 340073 | DUKE HEALTH RALEIGH HOSPITAL | NC | 2023 | Epic |
| 340187 | DAVIE MEDICAL CENTER | NC | 2023 | Epic |
| 390104 | UPMC KANE | PA | 2023 | TruBridge |
| 390326 | ST LUKE'S HOSPITAL - ANDERSON CAMPUS | PA | 2023,2024 | Epic |
| 390337 | GEISINGER MEDICAL CENTER MUNCY | PA | 2023,2024 | Epic |
| 400126 | HOSPITAL METROPOLITANO DE SAN GERMAN | PR | 2023 | Infomedika |
| 400136 | HOSPITAL SAN ANTONIO INC | PR |  |  |
| 430099 | REHAB & CRITICAL CARE HOSPITAL OF THE BLACK HILLS | SD |  |  |
| 431317 | LANDMANN-JUNGMAN MEMORIAL HOSPITAL - CAH | SD | 2023,2024 | MEDITECH |
| 440001 | UNICOI COUNTY  HOSPITAL | TN | 2023 | Epic |
| 440010 | WAYNE MEDICAL CENTER | TN | 2023 | Oracle Health |
| 440217 | BAPTIST MEMORIAL HOSPITAL-COLLIERVILLE | TN |  |  |
| 450143 | ASCENSION SETON SMITHVILLE | TX | 2023 | Oracle Health |
| 450271 | MEDICAL CITY DECATUR | TX | 2023 | Altera Digital Health |
| 450411 | EASTLAND MEMORIAL HOSPITAL | TX | 2024,2023 | Oracle Health |
| 450498 | STEPHENS MEMORIAL HOSPITAL | TX | 2023 | athenahealth |
| 450827 | KELL WEST REGIONAL HOSPITAL | TX | 2024,2023 | MEDITECH |
| 451397 | BAYLOR SCOTT & WHITE HOSPITAL BRENHAM | TX | 2023,2024 | Epic |
| 510031 | SAINT FRANCIS HOSPITAL | WV | 2023 | MEDITECH |
| 520215 | MARSHFIELD MEDICAL CENTER - RIVER REGION | WI | 2024,2023 | Oracle Health |
| 530011 | MEMORIAL HOSPITAL SWEETWATER COUNTY | WY | 2023 | Oracle Health |
| 530032 | EVANSTON REGIONAL HOSPITAL | WY | 2023 | MEDHOST |
| 530037 | NORTH PLATTE VALLEY MEDICAL CENTER | WY |  |  |
| 670143 | ASCENSION SETON BASTROP | TX | 2023 | Oracle Health |
| 670259 | CAPROCK HOSPITAL | TX | 2023 | Azalea Health |
| 670280 | NORTH HOUSTON SURGICAL HOSPITAL LLC | TX |  |  |

Total: 63

### CMS PI file vs ONC linkage for the same year

Where both cover a year, the CMS Provider Data Catalog release is newer than the snapshot ONC linked; hospitals whose CEHRT ID differs between the two get both IDs in `hospital_ehr` (attest_source tells them apart).

| year | CMS PI attestations | same CEHRT ID in ONC | hospital in ONC, different CEHRT ID | hospital not in ONC that year |
|---|---|---|---|---|
| 2024 | 4041 | 4018 | 20 | 3 |

### Hospital-years with two CEHRT IDs

| year | hospital-years | example |
|---|---|---|
| 2024 | 19 | 320004 |

### Attestation flag

Rows where the hospital did *not* meet PI criteria but still reported a CEHRT ID are kept; the flag is in staging only for now.

| year | meets PI criteria | hospitals |
|---|---|---|
| 2023 | yes | 3866 |
| 2023 |  | 180 |
| 2024 | yes | 3931 |
| 2024 |  | 127 |

## 7. Enrichment coverage

One row per enrichment table: rows loaded (all CCNs, including units and non-hospital providers), distinct CMS hospitals covered, and that as a share of the CMS hospital list. Empty rows mean the source has not been run (`ehr-atlas enrich-all`).

| table | rows | distinct CCNs | % of CMS hospitals |
|---|---|---|---|
| chpl_products (in hospital_ehr) | 0 |  |  |
| clinician_affiliations | 0 | 0 | 0 |
| fhir_endpoints (matched) | 0 | 0 | 0 |
| hospital_clinician_vendor | 0 | 0 | 0 |
| hospital_cost_reports | 0 | 0 | 0 |
| hospital_enrollment | 0 | 0 | 0 |
| hospital_geo (street or ZIP) | 0 | 0 | 0 |
| hospital_measures | 0 | 0 | 0 |
| hospital_medicare_inpatient | 0 | 0 | 0 |
| hospital_npi | 0 | 0 | 0 |
| hospital_pos | 0 | 0 | 0 |
| hospital_service_area | 0 | 0 | 0 |
| hospital_systems | 0 | 0 | 0 |
| hospital_vendor_year_mu (MU-era hospital-years) | 0 | 0 | 0 |
| practice_locations | 0 |  |  |
| practice_vendor (MIPS groups, with a vendor) | 0 |  |  |
| practices (D&C organisations matched, with a vendor) | 0 |  |  |
| practices (Lantern-only, source = lantern) | 0 |  |  |

### Geocoding

_none_

### Clinician-side vendor vs inpatient vendor

Rank-1 vendor among each hospital's affiliated clinicians (ONC clinician CEHRT PUF × Facility Affiliation) against the hospital's own primary vendor. Disagreement is expected where the medical staff runs an ambulatory system.

_none_

### FHIR endpoint vendor vs inpatient vendor (matched hospitals)

_none_

### Developer names in enrichment sources not in `config/vendor_map.csv`

_none_

### Certified products in use whose CHPL status is not Active

_none_

