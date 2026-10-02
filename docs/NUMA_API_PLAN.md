# Plan d’API : mémoire NUMA et tâches empruntées

Statut : proposition de conception partiellement implémentée. Les signatures
ci-dessous restent des cibles de travail, sauf indication contraire. Linux est
la première cible.

Déjà disponible sous Linux : `weave::memory::buffer::NumaBuffer<T>`, avec
`try_with_capacity`, `try_push`, `try_new` pour `T: Default`, les accès par slices
et `NumaPolicy::Bind`. Le buffer est `Send` si `T: Send` et `Sync` si `T: Sync`.
Sa capacité est fixe ; les capacités nulles et les types de taille nulle sont
rejetés. `MemoryPolicy`, `Preferred` et `try_from_fn` ci-dessous restent proposés.

## 1. Objectif et état actuel

Weave découvre la topologie, construit un `WorkerLayout`, épingle les workers et
ordonne leurs victimes de vol par proximité. `StealStats` mesure les transferts
entre workers, mais ne mesure pas le placement des pages ni le trafic mémoire.

La prochaine étape relie trois informations distinctes :

1. la politique demandée pour une allocation ;
2. le placement observé de ses pages à un instant donné ;
3. le nœud préféré pour exécuter une tâche qui utilise ces données.

Une référence Rust, un `move` ou un emprunt dans `scope` ne déplacent pas les
pages physiques. Les workers restent épinglés ; le scheduler rapproche d’abord
les tâches des données en choisissant les workers appropriés.

`WorkerLocal<T>` reste indépendant : il fournit une valeur par worker, mais son
initialisation actuelle a lieu sur le thread appelant et ne garantit pas une
allocation NUMA locale. Le nouveau buffer n’est pas du TLS.

## 2. Décisions pour la première version

- Allocation dédiée et propriétaire ; aucune adoption automatique d’un `Vec`.
- Préférence NUMA facultative sur les tâches, y compris celles de `scope`.
- Aucune copie, migration ou inspection implicite des captures d’une closure.
- Pas de contrainte stricte d’exécution dans la première version : les tâches
  restent exécutables à distance pour conserver la progression.
- APIs actuelles conservées : sans option, comportement historique.
- Erreur explicite si une opération mémoire Linux est indisponible ou refusée.
- Aucun changement de politique mémoire globale du processus.

## 3. Allocation propriétaire

Module public proposé : `weave::memory`.

```rust
pub enum MemoryPolicy {
    Bind(NumaNodeId),
    Preferred(NumaNodeId),
}

pub struct NumaBuffer<T> { /* allocation dédiée, longueur, politique */ }

impl<T> NumaBuffer<T> {
    pub fn try_from_fn(
        len: usize,
        policy: MemoryPolicy,
        init: impl FnMut(usize) -> T,
    ) -> Result<Self, MemoryError>;

    pub fn as_slice(&self) -> &[T];
    pub fn as_mut_slice(&mut self) -> &mut [T];
    pub fn policy(&self) -> MemoryPolicy;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}
```

`Bind` demande une politique limitée au nœud choisi. `Preferred` permet un repli
du système. Aucun des deux ne constitue une observation des pages réellement
résidentes ni une promesse de placement immuable.

Backend prévu : mapping anonyme privé via `mmap`, application de `mbind` avant
l’initialisation, puis destruction des éléments et `munmap`. L’initialiseur
s’exécute sur le thread appelant ; c’est la politique du mapping qui guide
l’allocation physique. Les écritures matérialisent les pages. Un simple mapping
ou une lecture de zéros ne suffit pas à démontrer leur placement.

La première tranche doit être un buffer d’octets initialisés, avant de
généraliser à `T`. La version générique doit traiter explicitement :

- multiplication de taille et arrondi aux pages avec débordements vérifiés ;
- limite `isize::MAX`, alignement et types suralignés ;
- longueur nulle et types de taille nulle ;
- destruction exacte des éléments déjà initialisés si `init` panique ;
- libération du mapping sur échec de configuration ;
- `Send` seulement si `T: Send`, `Sync` seulement si `T: Sync` ;
- aucune référence `&[T]` avant initialisation complète.

Une erreur de syscall peut devenir `MemoryError::Os`. En revanche, un manque
de mémoire au moment d’une faute de page ne devient pas nécessairement un
`Result` Rust : documenter les limites liées à l’overcommit et à l’OOM.

## 4. Localité des tâches

```rust
pub enum TaskLocality {
    Any,
    PreferNode(NumaNodeId),
}

pub struct TaskOptions {
    pub priority: Priority,
    pub locality: TaskLocality,
}
```

Valeur par défaut : priorité normale et `Any`. Ajouter les points d’entrée
`spawn_with_options` et `submit_with_options` au pool et au scope, et un setter
de localité sur `Job`. Les méthodes existantes délèguent avec `Any`.

`PreferNode` est un conseil : nœud sans worker, topologie inconnue, préférence
périmée ou nœud inexistant ne bloquent pas la tâche ; utiliser le chemin ordinaire.
La validité d’un nœud pour une allocation est en revanche contrôlée par la
couche mémoire et Linux. CPU autorisés et nœuds mémoire autorisés sont deux
restrictions distinctes, à ne pas dériver l’une de l’autre.

Une tâche capturant plusieurs buffers ne possède pas forcément un nœud idéal.
L’appelant choisit le jeu de données dominant ou `Any`. Pas d’analyse automatique
des captures ni de pondération cachée dans cette version.

## 5. Données empruntées et scopes

Signatures cibles, avec les mêmes durées de vie que les méthodes actuelles :

```rust
impl<'scope, 'env: 'scope> Scope<'scope, 'env> {
    pub fn spawn_with_options(
        &'scope self,
        options: TaskOptions,
        f: impl FnOnce() + Send + 'scope,
    );

    pub fn submit_with_options<T: Send + 'scope>(
        &'scope self,
        options: TaskOptions,
        f: impl FnOnce() -> T + Send + 'scope,
    ) -> JoinHandle<T>;
}
```

Exemple conceptuel avec une allocation existante :

```rust,ignore
// node provient d'une observation ou d'une connaissance de l'application.
let options = TaskOptions {
    priority: Priority::Normal,
    locality: TaskLocality::PreferNode(node),
};
pool.scope(|scope| {
    scope.spawn_with_options(options, || process(&data));
});
// data n'a été ni copiée, ni migrée, ni transférée au pool.
```

Pour des écritures parallèles, découper l’emprunt mutable avec `split_at_mut`
ou `chunks_mut`. Les morceaux doivent rester disjoints même si leurs pages
physiques sont partagées. Une préférence NUMA n’autorise jamais un alias mutable.

Pour un `NumaBuffer`, emprunter ses slices comme celles d’un `Vec`. Sa politique
peut servir d’indice pour la tâche ; une observation reste nécessaire si l’on
veut connaître le placement réel, notamment avec `Preferred`.

Garanties à conserver : attente de tous les descendants, y compris en cas de
panique ; destruction des captures avant déclaration de fin ; aucun emprunt
stocké au-delà du scope. Les métadonnées du `Job` possèdent seulement la
préférence, jamais un pointeur vers les données empruntées.

La localité n’est pas héritée implicitement par les tâches enfants dans la V1.
Les méthodes historiques de `join`, `install` et des itérateurs produisent `Any`.
Une propagation explicite pourra venir ensuite : une tâche enfant peut utiliser
d’autres données que son parent.

## 6. Observer les données déjà allouées

API cible en lecture seule :

```rust
pub fn query_placement<T>(data: &[T]) -> Result<PlacementReport, MemoryError>;
```

`PlacementReport` contient les nombres de pages par nœud, les pages non
résidentes et les échecs par page. Ne pas fabriquer un nœud unique lorsqu’une
allocation est distribuée. Le rapport précise sa couverture : V1 exhaustive,
échantillonnage éventuel explicite plus tard.

Le backend utilise `move_pages` en mode observation (`nodes = NULL`). Les
statuts de chaque page doivent être interprétés indépendamment du résultat
global. Ne pas écrire dans les données pour forcer leur résidence.

Le rapport est un instantané, pas une réservation. Les pages couvrant les bords
de la slice peuvent aussi contenir d’autres données ; leur statut décrit la
page entière. Pour `Vec<String>`, observer la slice localise les descripteurs
`String`, pas les allocations des caractères. Les objets indirects nécessitent
des observations séparées. Un intervalle vide ou de taille nulle ne contient
aucune page à interroger.

## 7. Migration : opération séparée et ultérieure

Ne pas proposer initialement `migrate(&mut [T])` pour une allocation quelconque :
l’exclusivité d’un emprunt Rust ne signifie pas propriété exclusive des pages.

Commencer par une opération explicite sur le mapping dédié du buffer, hors des
scopes qui l’empruntent. Son contrat devra distinguer :

- migration des pages présentes ;
- changement de politique pour les allocations futures ;
- succès partiel, pages non migrées et causes ;
- maintien des adresses virtuelles et absence de rollback automatique.

L’API de rapport de migration reste à concevoir avant cette phase. Un échec ne
doit pas invalider le buffer : certaines pages peuvent déjà avoir changé de nœud.
La copie vers un nouveau buffer reste une alternative explicite et plus simple.

## 8. Intégration scheduler

Conserver la correspondance worker → nœud du layout dans les données du pool.
Une tâche préférant un nœud est dirigée vers une file d’injection de ce nœud
lorsque des workers y existent ; l’absence de préférence garde les files actuelles.
Une soumission depuis un worker d’un autre nœud doit aussi suivre ce routage.

À priorité égale, proposition de première politique : file locale, injection du
nœud courant, injection globale, victimes du `StealPlan`, injections distantes.
La priorité reste le premier critère : une tâche distante de priorité haute
précède une tâche locale de priorité basse. Le repli distant évite qu’une
préférence empêche l’équilibrage ; il ne garantit pas l’absence de famine sous
un flux continu de travail local ou de haute priorité.

Lors du vol, toutes les tâches restent éligibles dans la V1. La préférence agit
d’abord sur l’attribution, et le plan existant sur l’ordre des victimes. Filtrer
ou réordonner les tâches d’une victime selon leurs données sera une phase
distincte, avec un budget de recherche borné et des tests de progression.

Tous les chemins doivent suivre ces règles : worker_loop, aide pendant join,
attente de scope, install et soumissions imbriquées. Vérifier également les
réveils : un simple notify_one ne garantit pas de réveiller un worker du nœud
ciblé. Commencer avec une stratégie correcte, puis mesurer son coût.

Ne pas déduire la localité des données de `StealStats`. Prévoir séparément des
compteurs d’exécution avec préférence satisfaite, non satisfaite, ou inconnue.
Ils ne sont toujours pas une mesure du trafic mémoire réel.

## 9. Erreurs, portabilité et restrictions

`MemoryError` doit distinguer plateforme non supportée, taille/alignement
invalide, sélection de nœud invalide et erreur système conservant `io::Error`.
Un refus de syscall par un conteneur n’est pas équivalent à l’absence de NUMA.
Les erreurs par page appartiennent au rapport ; un échec global à `Result`.

Encapsuler les appels Linux dans `memory/linux.rs`. Vérifier les bindings
disponibles dans `libc` avant de choisir entre syscalls et liaison à libnuma ;
ne pas coder de numéros de syscall en dur. Construire des masques de nœuds
dimensionnés selon les identifiants, qui peuvent être non contigus.

## 10. Petites étapes et critères de validation

1. Buffer d’octets privé : mapping, politique, initialisation et libération.
   Tester calculs de taille, nettoyage sur erreur et refus système.
2. Généralisation à `T` : alignement, ZST, initialisation partielle, Drop,
   Send/Sync. Auditer chaque bloc unsafe avant exposition publique.
3. TaskOptions et scope : emprunts partagés/mutables, résultats empruntés,
   tâches descendantes, paniques ; conserver les tests compile_fail de lifetime.
4. Injection par nœud : tests synthétiques de routage, priorités, repli sans
   worker local, aide pendant join/scope et réveil de workers endormis.
5. Observation Linux des slices ordinaires et des buffers : placements mixtes,
   pages absentes, erreurs partielles, permissions. Vérification matérielle
   multi-NUMA séparée des tests déterministes ; signaler explicitement les skips.
6. Mesures comparables avec les mêmes calculs, workers et données.
7. Migration explicite de mappings dédiés, puis étude des allocations externes.

Les tests sur une machine mono-NUMA ne prouvent pas le bon placement distant.
Les tests unitaires de politiques doivent pouvoir fonctionner sans syscalls.

## Références Linux

- [mbind(2)](https://man7.org/linux/man-pages/man2/mbind.2.html) : politique
  d’une plage mémoire et application aux pages allouées lors des écritures.
- [move_pages(2)](https://man7.org/linux/man-pages/man2/move_pages.2.html) :
  interrogation du placement et migration avec statuts par page.
- [set_mempolicy(2)](https://man7.org/linux/man-pages/man2/set_mempolicy.2.html) :
  politique du thread, distincte de la politique d’un mapping dédié.
