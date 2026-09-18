# Weave

Une bibliothèque Rust de calcul parallèle : pool de threads, véritable vol de tâches
entre workers, priorités, tâches empruntant des données, itérateurs parallèles et
description de la topologie matérielle. Elle utilise uniquement la bibliothèque
standard.

## Démarrage

```rust
use weave::{ThreadPoolBuilder, iter::*};

let pool = ThreadPoolBuilder::new().num_threads(4).build();
let mut doubles = vec![0; 10_000];
pool.install(|| (0..10_000).parallelize().map(|n| n * 2).fill(&mut doubles));
assert_eq!(doubles[42], 84);

let ticket = pool.submit(|| 6 * 7);
assert_eq!(ticket.join().unwrap(), 42);
```

Lancer la démonstration complète : `cargo run --example tour`. Sous Linux,
`cargo run --example topology` affiche les CPU logiques, cœurs physiques,
packages et nœuds NUMA découverts.
Le [rapport de réalisation](docs/RAPPORT_REALISATION.md) explique le projet
pour un lecteur non spécialiste et propose un déroulé de présentation.

## Choisir une opération

- `spawn(closure)` lance une tâche autonome ; `spawn(Job::new(...).set_priority(...))`
  accepte aussi une tâche configurée.
- `submit(closure)` fournit un `JoinHandle<T>`.
- `handle.join()` renvoie `std::thread::Result<T>`, avec le contenu original
  de la panique en cas d'échec. Abandonner le handle ne supprime pas la tâche.
- `pool.join(gauche, droite)` renvoie un couple et attend les deux branches,
  même si l'une échoue. La branche droite s'exécute sur le thread appelant.
- `pool.scope(|s| ...)` permet d'emprunter des variables extérieures :
  `s.spawn(...)` et `s.submit(...)` finissent avant la sortie du scope.
- `pool.install(...)` exécute le code dans ce pool. Les itérateurs utilisent
  le pool du worker courant ; en dehors d'un pool, ils s'exécutent séquentiellement.

Les variantes `spawn_with_priority` et `submit_with_priority` existent sur le pool
et sur les scopes. `High` précède `Normal`, qui précède `Low`, parmi les tâches
en attente. Une tâche en cours n'est pas interrompue. Un flux continu de haute
priorité peut retarder indéfiniment les autres priorités.

## Données et itérateurs

Importer `weave::iter::*`, puis utiliser :

- `(0usize..1000).parallelize()` ;
- `slice.iter_parallel()`, `slice.iter_parallel_mut()` ;
- `slice.chunks_parallel(taille)`, avec conservation du dernier morceau incomplet ;
- les adaptateurs paresseux `map`, `filter`, `filter_map` et `enumerate` ;
- les opérations terminales `for_each`, `fold`, `reduce`, `collect`, `count`,
  `sum`, `min`, `max`, `find`, `find_first`, `find_any`, `extend` et `fill`.

`map` est paresseux : le calcul démarre avec une opération terminale.
`collect()` renvoie un `Vec` ordonné. `fill(&mut sortie)` exige exactement
la bonne taille et écrit directement dans les sous-tranches, sans buffer intermédiaire.
Une panique peut laisser une sortie partiellement modifiée.

`fold(identité, opération, combinaison)` calcule un résultat par morceau puis
les réunit. L'identité doit être neutre, et la combinaison associative et cohérente
avec l'opération. Exemple : `fold(0, |a, b| a + b, |a, b| a + b)`.
`reduce(opération)` renvoie `None` sur une entrée vide. Les opérations à effets
de bord n'ont pas d'ordre d'exécution garanti. Les calculs flottants peuvent varier
avec le regroupement des opérations.

`IndexedParallelIterator` expose `len`, `is_empty`, `split_at` et
`into_sequential`. Les ranges supportées sont les intervalles exclusifs de
`usize`. Les extensions de slices sont utilisables sur les `Vec` et tableaux
par leur conversion automatique en slice.

`ChunksAligned` signifie « découpé aux frontières des morceaux » :
cela ne garantit pas un alignement mémoire SIMD.

## Topologie matérielle

```rust,no_run
use weave::topology::Topology;

let topology = Topology::discover()?;
println!("CPU logiques : {}", topology.logical_cpu_count());
println!("Cœurs physiques : {}", topology.physical_core_count());
println!("Packages : {}", topology.package_count());
println!("Nœuds NUMA : {}", topology.numa_node_count());
# Ok::<(), weave::topology::TopologyError>(())
```

La découverte est actuellement disponible sous Linux et lit les informations
exposées par `sysfs` dans `/sys/devices/system`. Seuls les CPU en ligne sont
inclus. Sur une machine sans interface NUMA exposée, Weave représente la mémoire
uniforme par un nœud synthétique d'identifiant zéro. Sur les autres plateformes,
`Topology::discover()` renvoie `TopologyError::UnsupportedPlatform`.

Cette API décrit passivement les relations entre CPU logiques, cœurs physiques,
packages et nœuds NUMA. Elle ne choisit pas le nombre de workers, ne fixe pas leur
affinité, ne place pas la mémoire et ne modifie pas encore la politique de vol de
tâches. Elle constitue la couche de découverte nécessaire à une future
ordonnance topology-aware.

## Stockage par worker

```rust
use weave::{ThreadPoolBuilder, WorkerLocal, iter::*};
let pool = ThreadPoolBuilder::new().num_threads(2).build();
let compteurs = WorkerLocal::new(&pool, || 0usize);
pool.install(|| (0..2000).parallelize().for_each(|_| {
    compteurs.with(|n| *n += 1);
}));
assert_eq!(compteurs.into_inner().iter().sum::<usize>(), 2000);
```

Le stockage appartient à un pool précis. `try_with` signale un accès depuis un
autre pool, un emprunt récursif ou une valeur empoisonnée par une panique.
Éviter de conserver un emprunt `with` pendant une attente coopérative : une tâche
réentrante peut vouloir accéder au même emplacement. Cet accès est détecté et
échoue immédiatement au lieu de bloquer.

## Panique et arrêt

- Une panique de `submit` est renvoyée par son handle.
- Une panique de `spawn` est signalée par le hook standard de Rust ; le worker continue.
- `join` attend les deux branches puis propage une panique (celle de gauche si les deux échouent).
- `scope` attend tous ses descendants avant de propager une panique.
  Un échec de `s.submit` récupéré par `join` appartient à l'appelant ;
  s'il n'est pas récupéré avant la fin du scope, le scope panique.
  Une panique du corps du scope a priorité sur celles des tâches.
- Détruire le pool depuis l'extérieur attend les travaux acceptés.
  Depuis l'un de ses propres workers, l'arrêt est demandé et les workers terminent
  en arrière-plan pour éviter d'attendre le thread courant.

Ces garanties concernent les paniques Rust avec déroulement de pile
(`panic=unwind`). Elles ne peuvent pas récupérer `panic=abort`, l'arrêt du
processus ou une tâche qui ne termine jamais. L'attente coopérative concerne les
opérations de Weave ; un verrou ou une réception bloquante fournis par l'utilisateur
peuvent toujours bloquer un worker.

## Architecture et limites

Chaque worker possède trois files locales, une par priorité. Il prend ses tâches
récentes d'un côté et vole les anciennes tâches des autres workers de l'autre.
Les appels extérieurs alimentent une file globale par priorité.
Un mutex commun protège l'ordonnanceur et la condition de réveil ; aucun code
utilisateur n'est exécuté sous ce verrou. C'est un vrai work-stealing, avec une
synchronisation centralisée : aucune prétention à égaler les performances de Rayon.

Le seuil de découpage est de 512 éléments. Le vol est mesurable avec
`pool.steal_count()`. Il n'existe pas encore de benchmark comparatif, de
garantie de temps réel, d'annulation, de pool redimensionnable, d'affinité CPU
ou de politique de vol utilisant la topologie découverte.
Deux effacements internes de durée de vie restent nécessaires aux tâches
empruntées ; leurs invariants sont commentés et testés, sans constituer une
preuve formelle de sûreté mémoire.

## Vérification

```text
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo test --doc
cargo test --release
cargo rustdoc --lib -- -D warnings -D missing-docs
cargo run --example tour
# Linux uniquement :
cargo run --example topology
```

La CI est configurée pour Windows et Linux. L'exemple `topology` compile sur
toutes les plateformes, mais sa découverte réelle ne réussit actuellement que
sous Linux.

## Migration du prototype

`ThreadPoolBuidler` reste un alias déprécié de `ThreadPoolBuilder`.
`num_thread`, `spawn_job` et `parallelize` restent disponibles.
`JoinHandle::join` renvoie désormais un `Result` au lieu d'une valeur brute.
`WorkerLocal::new` reçoit le pool propriétaire plutôt qu'un nombre.
`Scope` se crée exclusivement avec `pool.scope`.
`ChunksAligned::new` reçoit maintenant une slice et une taille.
L'ancien `chunk_aligned` générique est remplacé par l'extension de slice.
Le contrat du `Consumer` utilise un découpage à un index exact.
Les éléments publics sans fonction effective (`Worker`, `JobState`,
`JobId`/`Id`, `WaveError`) ont été retirés de l'API exposée.

Le dépôt ne déclare pas encore de licence de distribution. Le choix de cette
licence reste à faire par le propriétaire avant une publication.
