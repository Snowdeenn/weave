# Validation de la fusion du 3 octobre 2026

Fusion de la série locale terminée par 53d6abf avec origin/main à
1382f4a93fdac471c0c0805f90a706c10d274f49.
Les conflits README/ROADMAP conservent les cases et preuves de phase 0,
les descriptions corrigées du scheduler et les ajouts NUMA distants.
Le plan NUMA, son exemple et Send/Sync conditionnels sont intégrés.

Les implémentations unsafe de NumaBuffer sont justifiées par la propriété
exclusive du mapping, sa validité indépendante du thread, la destruction de T
sur le destinataire lorsque T: Send, et les seuls accès partagés &T lorsque
T: Sync. Toutes les mutations sûres exigent &mut self. Le transfert de propriété
ne migre pas les pages et ne modifie pas la politique du mapping.

Un test de bornes positives (u64 Send/Sync, Cell Send) et deux doctests
compile_fail (Rc non-Send, Cell non-Sync) vérifient ce contrat.
La matrice couvre désormais quatre exemples et 145 tests déclarés.

[Windows](windows.log) : 84 tests fonctionnels avec toutes les features,
7 doctests, formatage, Clippy toutes features et rustdoc strict réussis.
[Linux WSL2](linux.log) : 138 tests fonctionnels avec toutes les features,
10 doctests, Clippy toutes features, rustdoc strict et les huit scénarios de
stress release réussis (graine 20261003, 200 cycles, 76 800 jobs).
Les quatre exemples compilent ; les benchmarks passent Clippy toutes features.

Les sept tests NUMA réels restent ignorés : cet hôte ne permet pas leur
validation. L'exemple numa_scope n'est pas exécuté sur cet hôte ; sa compilation
et les bornes de threads sont vérifiées. Aucun résultat de placement mémoire
physique ou d'exécution mbind réelle n'est revendiqué.

L'audit Miri et les mesures de phase 0 restent ceux de la source archivée avant
fusion. Le code de scheduling et de durée de vie n'est pas modifié par cette
fusion ; les appels système NUMA restent exclus de Miri comme documenté.
La mesure comparative historique n'est pas remplacée par cette validation.
