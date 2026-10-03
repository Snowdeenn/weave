# Roadmap de Weave

Weave vise à devenir une bibliothèque Rust de parallélisme CPU sûre, ergonomique et performante : thread pool, work stealing, parallélisme structuré, itérateurs parallèles et prise en compte de la topologie matérielle. Les priorités, les budgets temporels et les graphes de dépendances doivent rester des primitives générales, utilisables aussi bien pour un moteur de jeu que pour la simulation, le rendu ou le traitement de données.

L'objectif de performance est de devenir compétitif avec Rayon sur des charges identifiées et reproductibles. Il ne constitue ni une promesse de supériorité universelle, ni une obligation de reproduire toute son API. Weave reste un moteur de calcul CPU ; l'interopérabilité async ne doit pas le transformer en executor async.

## Lecture et suivi

**État de référence :** dernier bilan du dépôt rapporté dans la discussion du 2 octobre 2026, portant sur `main` au commit `f0f81d3` du 1er octobre. Les cases cochées ci-dessous reprennent les fonctionnalités décrites comme présentes dans ce bilan ; elles ne constituent pas un nouvel audit du code, de sa sûreté ou de ses performances. La phase 0 prévoit leur vérification.

- `[x]` : fonctionnalité signalée comme présente dans cet état de référence.
- `[ ]` : travail restant, validation à effectuer ou piste à décider.
- **Partiel** : une base existe, mais le contrat ou l'intégration reste à compléter.
- **P0** : sûreté, correction et mesure ; préalable aux changements structurants.
- **P1** : cœur performant et API courante.
- **P2** : fonctionnalités avancées et intégration matérielle.
- **P3** : exploration à justifier par les usages et les mesures.

Les phases donnent un ordre de dépendance, sans date ni pourcentage d'avancement. Un jalon est terminé lorsque ses critères de sortie sont satisfaits. Les noms d'API à venir sont des propositions, à confirmer avant stabilisation.

## 1. Socle déjà présent

### Runtime et jobs

- [x] `ThreadPool` et `ThreadPoolBuilder`.
- [x] Nombre de workers automatique, `.num_threads(n)` et nommage des threads.
- [x] `spawn`, `submit` avec `JoinHandle<T>`, `join`, `install` et `scope`.
- [x] Tâches scoped pouvant emprunter des données non-`'static`.
- [x] Récupération des résultats et propagation des panics.
- [x] Arrêt propre du pool.
- [x] Parallélisme imbriqué et attente coopérative : un worker en attente peut exécuter du travail.
- [x] `Job`, `JobId`, `IntoJob`, labels et `Priority::{High, Normal, Low}`.

### Scheduler et stockage local

- [x] Files locales par worker et file globale.
- [x] Work stealing réel : travail local en LIFO, vol des tâches anciennes.
- [x] Files séparées par priorité.
- [x] Statistiques de stealing et sélection des victimes tenant compte de la topologie.
- [x] `WorkerLocal<T>` associé au pool, avec contrôle des mauvais accès et de la réentrance.
- [x] Premiers usages de `CachePadded` pour limiter le false sharing.

**Partiel :** l'équité entre priorités reste à renforcer ; les accès à `WorkerLocal` restent protégés/synchronisés ; les files locales ne signifient pas que le mutex central du scheduler a disparu.

### Itérateurs parallèles

- [x] `ParallelIterator`, `IndexedParallelIterator`, `IntoParallelIterator` et `Consumer`.
- [x] Sources : ranges, slices et slices mutables.
- [x] Adaptateurs : `map`, `filter`, `filter_map`, `enumerate`.
- [x] Consommateurs : `for_each`, `fold`, `reduce`, `collect`, `count`, `sum`, `min`, `max`.
- [x] Recherche : `find`, `find_first`, `find_any`.
- [x] `fill`, `extend` et découpage en chunks.
- [x] Base `ChunksAligned`.

**Partiel :** le découpage en nombres d'éléments ne suffit pas à garantir l'alignement des adresses mémoire. La sémantique de `ChunksAligned` doit être clarifiée avant de promettre un usage SIMD aligné.

### Topologie et NUMA

- [x] Découverte des CPU logiques, cœurs physiques, packages et nœuds NUMA.
- [x] `WorkerLayout`, placement par CPU logique ou par cœur physique, affinity/pinning.
- [x] Stealing privilégiant le même nœud NUMA et statistiques locales/distantes.
- [x] `NumaBuffer<T>` et base d'allocation typée/alignée avec `mmap` et `mbind` sur les plateformes concernées.

**Partiel :** ces briques ne forment pas encore une politique NUMA globale coordonnant placement des workers, allocation et ordonnancement. La classification explicite P-core/E-core reste à réaliser.

## 2. Phase 0 — Fiabiliser le socle et établir les mesures · P0

### Contrats et sûreté

- [x] Vérifier l'inventaire ci-dessus contre une révision précise du dépôt et associer les fonctionnalités à leurs tests.
- [x] Documenter les invariants du scheduler : propriété des jobs, publication, exécution au plus une fois et achèvement de chaque tâche acceptée.
- [ ] Auditer les blocs `unsafe`, pointeurs TLS et durées de vie ; éliminer toute référence artificiellement `'static` non justifiée.
- [ ] Garantir qu'aucune tâche empruntant des données ne survit au retour de son `scope`, y compris pendant un panic ou un échec de soumission.
- [ ] Vérifier la destruction du pool, des handles et des closures, ainsi que la libération des captures sur tous les chemins.
- [ ] Tester les réveils concurrents, les périodes sans travail et les transitions vers l'arrêt du pool.
- [ ] Tester le parallélisme imbriqué avec un seul worker, plusieurs pools et des soumissions concurrentes externes.
- [ ] Définir les garanties d'équité entre priorités et ajouter des scénarios de starvation.
- [ ] Clarifier ordre des résultats, ordre de recherche et propriétés attendues des réductions ; documenter les différences possibles pour les flottants.

### Mesure initiale

- [ ] Construire la suite comparative décrite en section 9 avant de remplacer le scheduler.
- [ ] Enregistrer une référence reproductible : révision, compilateur, profil de compilation, matériel, système et configuration du pool.
- [ ] Mesurer séparément débit, latence, allocations et contention.

### Critères de sortie

- [ ] Chaque invariant critique dispose d'une explication et d'un test ciblé ; aucun défaut de sûreté ou blocage connu ne reste ouvert sans résolution.
- [ ] Les tests unitaires, d'intégration, de documentation et de stress du socle passent sur les plateformes annoncées.
- [ ] Les portions compatibles passent sous Miri ; les exclusions sont explicites.
- [ ] Un rapport initial Weave / Rayon / séquentiel est reproductible et conservé avec les résultats bruts.

## 3. Phase 1 — Retirer les goulots d'étranglement du scheduler · P1

### Files concurrentes et stealing

- [ ] Retirer le mutex central du chemin courant du scheduler.
- [ ] Introduire des deques concurrentes par worker, en évaluant d'abord une primitive éprouvée plutôt qu'une nouvelle implémentation lock-free.
- [ ] Introduire un injector global concurrent pour les producteurs externes.
- [ ] Rendre `push/pop` local aussi peu coûteux que possible et conserver la politique LIFO locale / vol des tâches anciennes.
- [ ] Ajouter le vol par lots et mesurer son effet sur l'équilibrage, les priorités et la localité.
- [ ] Adapter la sélection des victimes et les tentatives de vol à la charge observée.
- [ ] Préserver les priorités sans faire dépendre chaque opération d'un verrou commun.
- [ ] Mettre en œuvre une politique anti-starvation testable.

### Attente, réveil et cache

- [ ] Définir une stratégie de backoff : recherche de travail, spin borné, yield, puis parking.
- [ ] Réveiller un nombre approprié de workers en fonction du travail disponible ; éviter les réveils collectifs inutiles.
- [ ] Prouver le protocole de parking/réveil pour éviter les notifications perdues.
- [ ] Séparer les données fréquemment écrites par différents workers et mesurer le false sharing avant d'ajouter du padding.
- [ ] Auditer les ordres atomiques et documenter les relations de publication ; utiliser `Relaxed`, `Acquire` et `Release` seulement lorsque les invariants les autorisent.
- [ ] Réduire le partage de compteurs chauds, les copies de métadonnées et les allocations sur les chemins fréquents.
- [ ] Prévoir un chemin léger pour les jobs `Normal` sans options avancées ; mesurer le coût des fonctionnalités désactivées.

### Critères de sortie

- [ ] Le chemin local courant ne dépend plus du mutex central ; les synchronisations restantes sont documentées.
- [ ] Stress et modèles de concurrence couvrent vol simultané, dernière tâche d'une deque, publication, parking et shutdown.
- [ ] Aucune tâche acceptée n'est perdue ou exécutée deux fois ; aucune régression de sûreté ou d'équité n'est observée dans la suite de validation.
- [ ] Les mesures avant/après montrent les gains et les éventuelles régressions, notamment à un worker et sur petites charges.

## 4. Phase 2 — Optimiser fork/join et les itérateurs · P1

### `join`, allocations et `WorkerLocal`

- [ ] Créer un chemin rapide pour `join` : exécuter une branche localement et rendre l'autre disponible au vol.
- [ ] Intégrer l'attente coopérative au scheduler et réduire le coût des fork/join imbriqués.
- [ ] Réduire les allocations et le dispatch dynamique des petites tâches ; évaluer les alternatives à `Box<dyn FnOnce()>` dans les chemins appropriés.
- [ ] Évaluer des représentations de tâches scoped sur la pile uniquement avec une garantie démontrée de leur durée de vie jusqu'à l'achèvement.
- [ ] Optimiser `WorkerLocal<T>` vers un accès propriétaire sans mutex, avec exclusivité worker → slot et protection contre la réentrance.
- [ ] Préserver cette exclusivité lorsqu'une attente coopérative exécute une autre tâche sur le même worker.
- [ ] Mesurer le coût de `get_mut()` et la réutilisation de buffers/scratch allocators par worker.

### Découpage et pipelines

- [ ] Remplacer le seuil fixe de découpage signalé dans le bilan (`MIN_CHUNK_SIZE = 512`) par une politique mesurée et configurable si nécessaire.
- [ ] Tenir compte du nombre de workers, de la taille des données et du coût du travail.
- [ ] Introduire le lazy splitting : ne pas créer tout l'arbre de tâches à l'avance.
- [ ] Évaluer un découpage adaptatif influencé par la demande de travail et le stealing.
- [ ] Borner le nombre de tâches et conserver un chemin séquentiel efficace pour les petits lots.
- [ ] Privilégier les portions contiguës afin de préserver la localité des caches.
- [ ] Spécialiser les sources indexées et éviter allocations intermédiaires et dispatch dynamique dans les adaptateurs.
- [ ] Vérifier l'inlining, la fusion des pipelines et la vectorisation sur des boucles représentatives ; utiliser `#[inline]` en fonction des mesures.
- [ ] Clarifier `chunks_aligned` : renommer le découpage simple si nécessaire, ou fournir une vraie garantie d'alignement avec gestion du préfixe et du reste.

### Critères de sortie

- [ ] Le nombre de jobs et les allocations ne croissent pas à raison d'un job alloué par élément sur les pipelines courants.
- [ ] Les petites charges, les calculs déséquilibrés et le parallélisme imbriqué disposent de comparaisons avant/après.
- [ ] Les résultats restent corrects sur données vides, petites, non divisibles et mutables.
- [ ] Tout accès sans mutex et toute optimisation `unsafe` disposent d'invariants et de tests dédiés.
- [ ] L'alignement annoncé est vérifié sur les adresses réelles, les types concernés et les cas limites.

## 5. Phase 3 — Compléter l'API courante et le contrôle des jobs · P1

### Soumission et ergonomie

- [ ] `spawn_batch(...)` avec insertion groupée et réveils amortis.
- [ ] Définir le comportement d'une soumission groupée vide, interrompue ou concurrente avec l'arrêt du pool.
- [ ] Ajouter `weave::prelude::*` avec une sélection limitée des traits et types principaux.
- [ ] Évaluer une API globale telle que `weave::join(...)`, avec règles explicites d'initialisation et de choix du pool.
- [ ] Ajouter des labels dynamiques, par exemple `Arc<str>`, si leur coût et leur utilité sont justifiés.
- [ ] Compléter les exemples et documenter les chemins d'import recommandés.

### Annulation et groupes

- [ ] Introduire un token d'annulation coopérative et un état terminal explicite pour les tâches annulées.
- [ ] Ajouter `cancel_by_label("world/physics")` et définir la correspondance des labels ainsi que le sort des soumissions futures.
- [ ] Ajouter des groupes de jobs et l'annulation d'un groupe.
- [ ] Ajouter l'annulation via handle.
- [ ] Définir `drain_priority(Priority::Low)` : retrait, annulation ou attente des jobs concernés, puis implémenter le contrat retenu.
- [ ] Éviter l'extraction arbitraire de toutes les deques si un marquage coopératif suffit.
- [ ] Distinguer tâches en attente et tâches déjà démarrées : une closure en cours n'est pas interrompue de force.
- [ ] Garantir que l'annulation règle les handles, compteurs de scope et dépendances, et libère les captures.

### Erreurs et panics

- [ ] Définir `PanicPolicy` configurable par pool.
- [ ] Préciser la sémantique de `AbortAndPropagate` ou choisir un nom moins ambigu : annulation du travail restant et propagation ne signifient pas arrêt du processus.
- [ ] Décider de la représentation `TaskError` et de la pertinence de `MultiPanicError`.
- [ ] Définir la sélection ou l'agrégation lorsque plusieurs branches paniquent.
- [ ] Documenter les interactions entre panic, annulation, scopes et futurs DAG, ainsi que les limites avec `panic = "abort"`.

### `TryParallelIterator`

- [ ] Introduire `TryParallelIterator`, ou une API équivalente conservant les mêmes objectifs.
- [ ] Ajouter `try_for_each`, `try_fold` et `try_reduce`.
- [ ] Déclencher un arrêt coopératif à la première erreur observée et limiter les nouvelles tâches lancées.
- [ ] Documenter que du travail déjà démarré peut continuer et que l'erreur retournée n'est pas nécessairement la première dans l'ordre d'entrée.
- [ ] Tester plusieurs erreurs simultanées, les destructions de valeurs intermédiaires et l'interaction avec les panics.

### Interopérabilité async

- [ ] Implémenter `Future` pour `JoinHandle<T>` sans bloquer le thread qui appelle `poll`.
- [ ] Stocker/remplacer le `Waker` et traiter la course entre enregistrement et achèvement sans réveil perdu.
- [ ] Harmoniser le résultat de `.join()` et `.await`, y compris panic et annulation.
- [ ] Ajouter `is_finished()` et évaluer `try_join()`.
- [ ] Définir le comportement au drop du handle et après consommation du résultat.
- [ ] Fournir des exemples d'intégration async sans dépendance obligatoire à un runtime particulier.

### Critères de sortie

- [ ] Chaque API possède une documentation de ses états, erreurs et interactions avec le shutdown.
- [ ] Les courses achèvement/annulation/panic et `poll`/réveil sont couvertes par des tests ciblés.
- [ ] Aucun handle ni scope ne reste en attente à cause d'une tâche annulée.
- [ ] Les exemples de batch, annulation, itérateur faillible, prélude et `.await` compilent et passent en CI.
- [ ] Le surcoût sur une tâche ordinaire sans annulation ni télémétrie est mesuré.

## 6. Phase 4 — Observabilité et primitives avancées · P2

L'instrumentation minimale peut commencer dès les phases 0–1 pour guider les optimisations. Les scopes avancés dépendent des contrats d'achèvement, d'annulation et de panic de la phase 3.

### Tracy et télémétrie

- [ ] Ajouter une intégration Tracy optionnelle sous feature Cargo et une activation telle que `enable_tracy_telemetry(true)`.
- [ ] Exposer durées et labels des jobs, identifiants des workers d'origine et d'exécution, priorités et scopes.
- [ ] Observer profondeur des files, workers actifs, temps idle, attente et tentatives de vol.
- [ ] Distinguer steals réussis/échoués et même nœud NUMA/nœud distant.
- [ ] Ajouter la visualisation des dépendances DAG et du temps par priorité si utile.
- [ ] Mesurer séparément les coûts sans instrumentation compilée, avec instrumentation inactive et avec capture active.

### `BudgetScope` et travail reporté entre frames

- [ ] Définir `scope_budget(Duration, ...)` / `BudgetScope` et son horloge de référence.
- [ ] Arrêter le lancement de nouveaux jobs lorsque le budget est consommé.
- [ ] Conserver les jobs non démarrés dans un propriétaire explicite et permettre leur reprise à la frame suivante.
- [ ] Réserver le travail reportable à des données possédées/`'static`, ou démontrer un autre modèle de durée de vie sûr.
- [ ] Maintenir le contrat distinct de `Scope<'scope>` : tous ses emprunts doivent cesser avant son retour.
- [ ] Définir le sort des tâches déjà lancées, le résultat de l'appel et la politique au drop du backlog.
- [ ] Combiner priorités, annulation et budget, avec protection contre l'accumulation indéfinie de travail.
- [ ] Documenter qu'un budget d'admission ne garantit pas une échéance temps réel stricte pour des tâches non préemptibles.

### `DagScope` et dépendances

- [ ] Ajouter `scope_dag`, `DagScope`, des identifiants de nœuds / `DagHandle` et `submit_after(&[a, b], ...)`.
- [ ] Maintenir des compteurs de dépendances et des listes de dépendants ; publier un nœud exactement une fois lorsque ses dépendances sont satisfaites.
- [ ] Définir la visibilité mémoire des résultats des prédécesseurs avant l'exécution des successeurs.
- [ ] Refuser les handles d'un autre graphe, les dépendances invalides et les cycles, ou les rendre impossibles par construction.
- [ ] Définir le comportement des descendants après erreur, panic ou annulation d'un prédécesseur.
- [ ] Intégrer les priorités et garantir l'achèvement de tous les nœuds, y compris ceux qui ne seront jamais exécutés.
- [ ] Pour un DAG scoped, garantir que rien ne survit aux emprunts ; séparer toute variante persistante.

### Critères de sortie

- [ ] Une trace permet d'expliquer un déséquilibre ou une attente sans modifier le code utilisateur.
- [ ] Un exemple multi-frame démontre reprise, annulation et destruction sûre du travail reporté.
- [ ] Les tests de budget couvrent budget nul, dépassement par une tâche longue et accumulation du backlog.
- [ ] Les tests DAG couvrent chaîne, losange, plusieurs racines, graphe vide, dépendances invalides et propagation des échecs.
- [ ] Aucun nœud n'est exécuté avant ses dépendances, deux fois, ou laissé en attente après un échec terminal.

## 7. Phase 5 — Intégration matérielle complète · P2

### Cœurs hétérogènes

- [ ] Détecter explicitement les classes performance/efficacité lorsque la plateforme le permet.
- [ ] Ajouter une politique telle que `.bind_to_p_cores()` et définir le comportement si l'information est indisponible.
- [ ] Permettre des politiques distinctes pour P-cores et E-cores sans supposer que tous les workers ont la même capacité.
- [ ] Évaluer la pondération de capacité, l'affectation par priorité et leur interaction avec le vol de tâches.
- [ ] Respecter les CPU réellement autorisés au processus et documenter les limites du pinning.

### NUMA cohérent à l'échelle du pool

- [ ] Définir `.numa_aware(true)` comme une politique coordonnant placement, mémoire et scheduler.
- [ ] Évaluer un injector par nœud NUMA et des files hiérarchiques.
- [ ] Définir une recherche privilégiant worker local, proximité de cœur si pertinente, même nœud, puis nœuds distants.
- [ ] Relier `WorkerLocal` et les allocations locales au nœud du worker.
- [ ] Contrôler le first-touch et documenter les interactions avec `NumaBuffer<T>`.
- [ ] Équilibrer localité et progression : permettre le vol distant lorsqu'un nœud manque de travail.
- [ ] Mesurer fréquence et coût des accès/steals distants, au-delà du simple nombre de tâches volées.
- [ ] Définir des replis corrects sur machines mono-NUMA, systèmes non pris en charge ou refus de politique mémoire par le système.

### Critères de sortie

- [ ] Les tests et mesures utilisent du matériel hybride et multi-NUMA réel ; la couverture indisponible est annoncée.
- [ ] Les gains ou pertes sont publiés pour topologie activée/désactivée, mémoire locale/distante et charges équilibrées/déséquilibrées.
- [ ] Les plateformes sans ces capacités gardent un comportement correct et documenté.
- [ ] Les modes avancés ne sont activés par défaut qu'après validation de leur coût et de leur robustesse.

## 8. Vision long terme · P3

Ces pistes restent à arbitrer ; leur présence n'est pas un engagement de version.

- [ ] Pool redimensionnable : gestion des slots `WorkerLocal`, tâches en vol, affinity et shutdown.
- [ ] Deadlines par job et politiques d'admission adaptées aux charges interactives.
- [ ] Migration et rééquilibrage NUMA tenant compte du coût de déplacement des données.
- [ ] Scheduler davantage conscient de la charge, de la capacité des cœurs et de la pression mémoire.
- [ ] Adaptateurs supplémentaires : `zip`, `chain`, puis éventuellement `flat_map`, `flatten`, `any` et `all`, après vérification de l'API existante et des besoins.
- [ ] Meilleure intégration des buffers alignés et traitements SIMD, sans garantie implicite liée au seul découpage.
- [ ] Exemples complets : simulation, rendu, traitement de données et pipeline de frame avec DAG/budget.
- [ ] Stabilisation progressive de l'API publique, politique de compatibilité, plateformes supportées et MSRV documentées.

**Critère de passage à l'implémentation :** chaque piste dispose d'un cas d'usage, d'un contrat, d'un coût attendu et d'un protocole de validation. Les extensions ne doivent pas compromettre le chemin courant des calculs simples.

## 9. Benchmarks et validation continus · P0 à P2

### Matrice de performance

| Axe | Scénarios à couvrir | Mesures principales |
| --- | --- | --- |
| Référence | Weave, Rayon et version séquentielle équivalente | Temps, débit, dispersion, accélération |
| Soumission | `spawn`, `submit`, batch, job vide et petit job | Coût par tâche, allocations, latence |
| Fork/join | `join`, récursion, scopes, parallélisme imbriqué | Surcoût, profondeur, progression à un worker |
| Itérateurs | `map`, `filter`, `reduce`, collecte ; calcul faible, moyen et lourd | Temps, allocations, granularité |
| Équilibrage | Jobs hétérogènes, arbres irréguliers, producteurs concurrents | Steals utiles/inutiles, contention, temps idle |
| Priorités | Mélange High/Normal/Low sous charge durable | Latence par priorité, progression des tâches basses |
| Scaling | 1, 2, 4, 8, 16… workers selon le matériel | Accélération, saturation, efficacité par worker |
| Mémoire | Données contiguës/dispersées, jeux tenant ou non en cache | Cache misses, bande passante, false sharing |
| Topologie | Affinity et topologie activées/désactivées, SMT, P/E | Temps, variabilité, répartition de charge |
| NUMA | Mono/multi-nœuds, first-touch, mémoire locale/distante | Débit, accès distants, coût des steals distants |
| Options | Annulation, télémétrie et métadonnées activées/désactivées | Surcoût marginal, taille et allocations des jobs |

### Protocole reproductible

- [ ] Utiliser les mêmes entrées, vérifier les résultats et rendre le travail observable pour éviter son élimination par le compilateur.
- [ ] Comparer des algorithmes équivalents avec le même nombre de workers et les mêmes contraintes de placement.
- [ ] Séparer création/destruction du pool et exécution en régime établi ; publier les deux lorsque pertinent.
- [ ] Inclure échauffement, répétitions, dispersion et conditions de charge de la machine.
- [ ] Enregistrer versions de Weave/Rayon/Rust, options de compilation, CPU, mémoire, OS, SMT et configuration NUMA.
- [ ] Publier résultats bruts, commandes et limites de mesure ; éviter toute conclusion universelle à partir d'une seule machine.
- [ ] Définir par benchmark des seuils de régression après mesure du bruit, puis suivre les tendances sur un environnement stable.
- [ ] Réserver les comparaisons quantitatives sensibles à des runners contrôlés ; utiliser la CI ordinaire pour vérifier que les benchmarks fonctionnent.

### Correction et concurrence

- [ ] Tests de stress longs avec graines reproductibles : soumissions, vols, panics, annulations et shutdown concurrents.
- [ ] Détection des tâches perdues, doublons, blocages, destructions multiples et captures non libérées.
- [ ] Tests aux limites : zéro tâche, un worker, très petites collections, forte récursion et plusieurs pools.
- [ ] Miri sur les portions `unsafe` compatibles, avec exclusions justifiées pour les appels système et primitives non pris en charge.
- [ ] Modèles Loom ciblés pour états de handle, publication des résultats, compteurs DAG et protocoles de réveil ; ne pas prétendre couvrir tout le runtime.
- [ ] CI de compilation, tests, documentation, formatage et lint, incluant les combinaisons de features supportées.
- [ ] Validation matérielle séparée pour affinity, P/E et NUMA.

## 10. Jalons de livraison

| Jalon | Périmètre | Condition de clôture |
| --- | --- | --- |
| Socle fiable — candidat 0.1 | API actuelle vérifiée, contrats, sûreté, documentation et mesures de référence | Critères de phase 0 satisfaits ; périmètre public et limitations documentés |
| Cœur compétitif | Scheduler, fork/join et itérateurs optimisés | Critères de phases 1–2 satisfaits ; rapport comparatif publié sans régression inexpliquée |
| API de contrôle | Batch, prélude, annulation, erreurs, itérateurs faillibles, interopérabilité async | Critères de phase 3 satisfaits |
| Exécution avancée | Tracy, budget et DAG | Critères de phase 4 satisfaits et exemples réalistes validés |
| Runtime sensible au matériel | P/E cores et politique NUMA intégrée | Critères de phase 5 satisfaits sur matériel représentatif |

Les numéros de versions suivants seront décidés selon la compatibilité des changements. Une première version utilisable ne dépend pas de toutes les fonctionnalités de long terme.

## 11. Règles de contribution à cette roadmap

- Relier chaque chantier engagé à une issue précisant le contrat, les dépendances et les critères de validation.
- Ajouter les preuves utiles à la revue : tests, invariants pour l'`unsafe`, mesures avant/après pour les optimisations.
- Mettre à jour les cases et l'état de référence après intégration, sans confondre présence d'une API et validation de ses garanties.
- Justifier les régressions acceptées et documenter les plateformes ou usages non couverts.
- Garder la sûreté et la correction prioritaires sur les gains de performance ; conserver les fonctionnalités spécialisées optionnelles lorsque possible.
