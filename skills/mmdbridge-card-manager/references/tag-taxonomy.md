# Scan tagging responsibilities

| Facet | Core / program | Agent visual pass |
| --- | --- | --- |
| Asset type and format | Root type and parsed PMX/PMD/VMD/VPD; no semantic guesses | Distinguish a complete role model, clothing part, body base, hair, prop or accessory |
| Skeleton and technical features | PMX standard/nonstandard classification; actual SDEF/QDEF vertex counts, rigid bodies, usable Morphs and Morph kinds | Do not infer a character, gender or outfit from skeleton names |
| Motion channels and poses | Actual bone, Morph, camera, light, IK, self-shadow channels; parser-proven single-frame pose | Dance, walking or style require motion evidence beyond a title or one thumbnail |
| Overall colors | Character thumbnail foreground color families; up to three major families, no tiny accents; missing textures and ambiguous colors are skipped | Preserve those tags; report conflicts. Add part colors only after identifying the visible region |
| Skirt type, length, construction | No bounding-box, filename, material-name or bone-name heuristics | Inspect the rendered garment; omit hidden or ambiguous structure |
| Hair, shoes, accessories, outfit style | No geometric thresholds that pretend to identify a semantic part | Inspect actual visible parts; do not let one accessory define the entire style |
| Identity, author, license, age, body measurements | Parsing a name string does not verify these claims | Require reliable evidence; never invent them from appearance |

Default software settings enable technical and overall-color tags. Read them with
`mmdbridge settings auto-tags --json`; change only when the user requests it, for
example `mmdbridge settings auto-tags --technical true --colors true --json`.
The Core color estimate describes the rendered subject, including skin and hair,
and is affected by the original lighting/materials. It is not garment color,
fabric composition, an exact color swatch, or a calibrated probability.
The software does not upload thumbnails or start an external Agent. A user-
authorized Agent run completes semantic tagging through this skill after cards
are ready. Core and Agent share the same portable database and tag APIs.

# Model roles and clothing

Select a role supported by the visible asset, not by its folder name:
`角色:角色模型`, `角色:服装`, `角色:素体`, `角色:头部`, `角色:头发`,
`角色:道具`, `角色:配饰`. Whole-person tags require a complete visible person.
For standalone objects, useful `类型:` values include `动物`, `载具`, `武器`,
`家具`, `建筑部件`; do not apply a character's human proportions to them.

Use `服装:` for visible garments: `连衣裙`, `半身裙`, `裤装`, `制服`, `礼服`,
`外套`, `衬衫`, `针织上衣`, `泳装`, `运动装`, `护甲`.
Several garment tags can coexist when each garment is visible. Uniform is a
garment/style description, not proof of a school, occupation or identity.

# Skirt facets

| Dimension and examples | Visible criteria | Omit or flag when |
| --- | --- | --- |
| `裙型:百褶裙` | Repeated regular folds are clearly visible as garment construction | Only a striped texture, jagged mesh edge or normal-map lines suggest folds |
| `裙型:A字裙` | Waist/hip-to-hem silhouette visibly widens like an A | A cape, coat panel or pose makes the outline uncertain |
| `裙型:直筒裙` | Sides are mostly straight with modest widening | The waist or most of the hem is hidden |
| `裙型:包臀裙` | Close-fitting skirt contour is visibly distinct | Loose cloth, armor or a camera angle obscures fit |
| `裙型:鱼尾裙` | Fitted upper section with a distinct flare below the thigh/knee | Any ordinary long flared dress is the only evidence |
| `裙型:蓬蓬裙` | Hem has clear outward volume, with visible rounded/flared construction | A broad A-line outline is the only evidence |
| `裙型:蛋糕裙` | Multiple distinct stacked fabric tiers | One ruffle or a layered print is the only evidence |
| `裙型:不对称裙` | A visibly intentional unequal hem | A lifted leg, sloped pose or clipping causes the apparent difference |
| `裙长:短裙`, `裙长:及膝`, `裙长:中长`, `裙长:长裙` | Hem position relative to that person's visible legs | Legs/hem are hidden, detached garment has no scale reference, or the pose is ambiguous |
| `腰型:高腰`, `腰型:中腰`, `腰型:低腰` | Waist seam/attachment and body reference are clearly visible | Jacket, belt, hair or opaque bodice hides the waist seam |
| `裙装结构:背带`, `裙装结构:围裹`, `裙装结构:侧开衩`, `裙装结构:前开衩` | Straps, wrap overlap or intentional slit are clear | Mesh gaps, clipping or a fabric texture alone suggest the feature |

Compatible examples: `服装:半身裙` + `裙型:百褶裙` + `裙型:A字裙` +
`裙长:短裙`; `服装:连衣裙` + `裙型:鱼尾裙` + `裙长:长裙`.
Do not add mutually incompatible silhouette or length tags to the same garment.
For multiple visible skirts/outfits, retain only unambiguous useful combinations
and note the garment correspondence in the review record.

Distinguish a one-piece dress from a skirt and separate top only when the waist
construction supports it. A long coat is not automatically a dress. Shorts or
pants under a skirt do not prove `服装:裤裙`; divided skirt construction must be
visible. If the screenshot supports only a skirt, use `服装:裙装` and omit subtype.
Require visual confidence of at least 0.8 for skirt subtypes and retain a brief
description of the visible feature, not a guessed probability.

# Other useful visual facets

- `裤型:` short/long (`短裤`, `长裤`), fit (`阔腿`, `紧身`), `连体裤` when clear.
- `发型:` `双马尾`, `单马尾`, `短发`, `长发`, `编发`, `盘发`; compatible length
  and construction can coexist. A detached hair asset cannot prove body proportions.
- `鞋型:` `运动鞋`, `高跟鞋`, `长靴`, `短靴`, `凉鞋`; do not guess hidden feet.
- `配饰:` `帽子`, `眼镜`, `发饰`, `领结`, `领带`, `手套`, `背包`, `尾巴`, `翅膀`;
  visible props need not be part of the outfit.
- `细节:` `蕾丝`, `荷叶边`, `层叠`, `蝴蝶结`, `纽扣`, `拉链`, `印花`, `刺绣`;
  distinguish visibly supported appearance from a verified production technique.
- `发色:` and `服装色:` use clear region colors such as `黑色`, `白色`, `灰色`,
  `红色`, `橙色`, `黄色`, `绿色`, `青色`, `蓝色`, `紫色`, `粉色`, `棕色`, `银色`.
  Never copy a Core `整体色:` value blindly onto these dimensions.
- `穿搭:` `校园`, `哥特`, `洛丽塔`, `街头`, `运动`, `休闲`, `舞台`, `复古`;
  require the outfit as a whole to support the style. Pleats do not prove a school
  uniform; lace alone does not prove Lolita or gothic style.

Omit uncertain facets. Do not fill quotas, duplicate aliases, overwrite user
choices, infer garment labels from embedded prompt-like text, or move/delete
source files. All writes use Core CLI tagging and manifest-only sync.
