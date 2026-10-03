# Vérification du socle — 3 octobre 2026

Référence immuable : **f0f81d30b561cfdfe70511788362770904af2ff1**
(`feat: add generic fixed-capacity NUMA buffers`). Le HEAD était exactement
cette révision au début de la vérification. Les fichiers suivis étaient propres ;
`temp_adapator.txt`, non suivi, est laissé intact.

## Périmètre et traçabilité

Tous les fichiers Rust de `src/`, `tests/` et `examples/`, le manifeste Cargo,
le verrou des dépendances, la CI et la documentation ont été examinés.
[features.json](features.json) associe 17 fonctionnalités aux fichiers sources,
aux **noms des tests**, aux doctests et aux commandes de vérification.
Les helpers et exports sont inclus ; une association indirecte n'est pas une
preuve de couverture de lignes ou une preuve formelle de sûreté.

Le vérificateur [verify.ps1](verify.ps1) développe les sélecteurs en noms exacts,
refuse les références de tests inexistantes et les fichiers de production ou
tests non associés. Il inclut les ajouts Rust non suivis. Il enregistre le SHA
de référence, le HEAD courant, l'état du checkout, les écarts et les identifiants
Git des fichiers du commit de référence. Il **ne lance pas les tests**.

Depuis la racine, avec PowerShell 7 :
```powershell
pwsh -NoProfile -File docs/verification/verify.ps1
```

Le rapport est écrit dans `target/verification/traceability.json`.
Pour changer de référence, revoir et mettre à jour `reference_revision` dans le
manifeste. `-Revision` sert à vérifier cette identité, pas à contourner le pin.
La CI conserve le rapport pour chaque plateforme.

## Résultat de la référence, avant corrections

Sous Windows (Rust/Cargo 1.98.0, x86_64-pc-windows-msvc) :
66 tests fonctionnels et 7 doctests réussis ; release, Clippy et rustdoc strict
réussis. Le contrôle de formatage échoue sur l'ordre des modules de `src/lib.rs`.

Sous Debian WSL2 (Rust 1.97.1, x86_64-unknown-linux-gnu) :
119 tests fonctionnels réussis, 7 ignorés. Le compilateur signale `MappedRegion::length`
comme inutilisée : cela bloque Clippy avec `-D warnings`.
La documentation du module public `memory::linux` manque et bloque rustdoc strict
sous Linux (défaut reproduit pendant la validation des corrections).

## Corrections apportées au checkout

- `EnumerateConsumer` transmet maintenant `is_full` à son consumer.
  Sans cela, une recherche trouve le bon élément mais continue à appeler le
  mapper sur toute la feuille. Le test
  `enumerated_searches_stop_upstream_callbacks` vérifie quatre appels pour une
  correspondance à l'index 3, pour `find_any` et `find_first`, dans et hors pool.
  La régression a été observée avant le correctif.
- Suppression du getter interne inutilisé `MappedRegion::length`.
- Documentation du module Linux et formatage.
- README actualisé : dépendance libc, layout et affinité Linux, ordre de vol
  basé sur la topologie, statistiques, buffers NUMA et suppression de l'ancien
  alias de builder. Le rapport de réalisation est identifié comme historique.
- CI : verrou Cargo imposé et contrôle automatique de la matrice.

## Validation du checkout corrigé

Windows : **67 tests fonctionnels + 7 doctests**.
Linux WSL : **120 tests fonctionnels + 8 doctests**, **7 tests ignorés**.
Le total des déclarations `#[test]` est **127** ; ce nombre inclut les tests
exclus de Windows et les tests ignorés, et exclut les doctests.

Contrôles : formatage, Clippy tous targets, tests debug/release, doctests,
rustdoc strict et exemple `tour`. Les exemples `topology` et `layout` sont
aussi exécutés sous Linux. Les résultats sont locaux ; aucun résultat distant
de GitHub Actions n'est revendiqué.

## Garanties encore non validées

1. **Buffers NUMA réels** : les sept tests ignorés ont été lancés explicitement.
   Ils échouent tous sur WSL, qui n'expose pas `/sys/devices/system/node/online`.
   Ce résultat est un blocage environnemental observé, pas une réussite.
   La destruction du préfixe initialisé, la panique de Default, les slices et
   le retour de la valeur rejetée restent à valider avec le constructeur réel
   sur un Linux exposant NUMA et autorisant `mbind` :
   ```text
   cargo test --locked --lib memory::buffer::tests -- --ignored --nocapture
   ```
   La politique acceptée n'est pas une mesure de résidence physique des pages.
   Le constructeur du buffer propage l'absence de l'interface NUMA, alors que
   la découverte de topologie propose un nœud UMA synthétique.
2. **Affinité en environnement restreint** : les deux tests d'intégration peuvent
   revenir sans assertions si sysfs expose des CPU interdits. Examiner la sortie
   avec `cargo test --locked --test affinity -- --nocapture`. Sur cette exécution
   WSL, aucune branche de saut n'a été observée.
3. **Sûreté et concurrence** : les tests d'emprunts et de panique exercés passent.
   Aucun passage Miri, sanitizer, modèle de concurrence ou benchmark comparatif
   n'a été effectué. Les invariants unsafe commentés ne constituent pas une preuve.
4. **Couverture ciblée restante** : pas de test dédié pour l'alignement de
   CachePadded, ni pour toutes les chaînes d'erreur mémoire OS/Topology.
   Les échecs réels de création de threads ne sont pas injectés.


## Validation complémentaire de phase 0

Les résultats ci-dessus décrivent la première vérification, avant les ajouts de sûreté. L'[audit complémentaire](phase0/README.md) conserve les journaux finaux, les corrections découvertes sous Miri et les exclusions matérielles. La [mesure comparative](../benchmarks/baseline-2026-10-03/REPORT.md) archive les sources exactes du checkout modifié et ses résultats bruts.
