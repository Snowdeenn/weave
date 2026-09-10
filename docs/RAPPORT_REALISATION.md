# Weave — rapport de réalisation

Rapport du 10 septembre 2026. Ce document est prévu pour une lecture autonome
et pour servir de support à une présentation devant un jury non spécialiste.

## 1. Le projet, expliqué simplement

Un ordinateur possède généralement plusieurs unités capables de travailler
en même temps. Pourtant, un programme classique peut leur donner les opérations
une par une. Weave propose de répartir des calculs indépendants entre plusieurs
travailleurs, appelés « threads », puis de récupérer leurs résultats.

Imaginez un atelier avec quatre employés et des milliers de pièces à traiter.
Le responsable doit distribuer les pièces, éviter qu'un employé reste sans
travail pendant qu'un autre est débordé, gérer les urgences et vérifier que
tout est terminé avant de fermer l'atelier. C'est le rôle de cette bibliothèque.

Le résultat de cette intervention est une implémentation des onze éléments
demandés, accompagnée de tests, d'une démonstration exécutable et d'une
documentation. Elle couvre l'ordonnancement, la récupération des résultats,
les emprunts de données et les opérations sur des collections.

## 2. Ce qui devait être réparé

Le prototype compilait, mais n'avait aucun test. Plusieurs défauts concernaient
directement son fonctionnement : des workers réveillés repartaient dormir,
la fermeture du pool pouvait attendre des threads en leur empêchant l'accès
au verrou nécessaire pour s'arrêter, et un pointeur pouvait désigner une adresse
qui n'hébergeait plus le pool.

Le vol de tâches annoncé n'avait pas de chemin opérationnel. Les tâches en
échec ne garantissaient pas la fin des attentes. Certains itérateurs perdaient
leur contenu en se transformant en sources vides. Le builder ignorait une option
si l'autre n'était pas également renseignée.

Le cœur du pool a donc été réécrit. L'état partagé vit maintenant dans un objet
à propriété partagée stable, sans pointeur brut vers le pool. Le réveil et les
files de tâches sont protégés par le même verrou. La fermeture libère ce verrou
avant d'attendre les threads. Les tâches empruntées sont attendues même quand
une branche échoue.

## 3. ThreadPoolBuilder : préparer l'atelier

Le builder est le formulaire de configuration. On choisit combien de workers
doivent travailler et le préfixe de leur nom. Chaque option fonctionne séparément.
Sans nombre explicite, Weave utilise le parallélisme disponible indiqué par le
système. Sans nom explicite, il utilise « weave ».

Un pool à zéro worker est rejeté : il ne pourrait accomplir aucune tâche.
La méthode `try_build` fournit une erreur exploitable si la configuration est
invalide ou si le système refuse de créer un thread. `build` est la variante
courte qui panique dans ce cas.

Le nom correct `ThreadPoolBuilder` est désormais public. L'ancienne faute
`ThreadPoolBuidler` reste temporairement acceptée avec un avertissement de
dépréciation pour faciliter la migration.

## 4. Le vrai work-stealing : rééquilibrer le travail

Chaque worker dispose de ses propres files de tâches. Lorsqu'il crée un
sous-travail, celui-ci rejoint sa file locale. Les tâches déposées depuis
l'extérieur rejoignent une file globale.

Un worker commence par chercher du travail disponible, en respectant les
priorités. Il prend les tâches récentes de sa propre file. S'il trouve du
travail chez un autre worker, il en prélève une tâche ancienne à l'autre
extrémité de la file : c'est le « vol de travail ».

Dans l'atelier, cela correspond à un employé disponible qui prend une pièce
dans la pile d'un collègue. Le travail n'est ni copié ni exécuté deux fois.
Une tâche est retirée de sa file au moment où elle est attribuée.

Un test vérifie ce mécanisme directement : un worker crée une tâche locale puis
attend sans aider. Un autre worker doit prendre cette tâche et signaler son
identité. Le test vérifie que les deux identités diffèrent et que le compteur
de vols augmente. Ce compteur est accessible par `steal_count`.

Les files sont protégées par un mutex commun. Cela simplifie la cohérence des
priorités et des réveils, mais peut limiter les performances quand énormément
de threads tentent de prendre de très petites tâches en même temps.

## 5. spawn, submit, join et scope : quatre manières de travailler

`spawn` dépose un travail autonome. L'appelant poursuit son activité sans
recevoir de résultat. C'est adapté à une opération dont seule l'exécution
importe, par exemple mettre à jour un compteur partagé.

`submit` dépose un travail et rend un ticket de récupération. L'appelant peut
continuer d'autres opérations, puis utiliser ce ticket pour récupérer la valeur
calculée ou l'échec. Par exemple, demander « combien vaut 6 × 7 ? » et obtenir 42.

`join` lance deux branches et les réunit. L'une est confiée au pool, l'autre
est exécutée par le thread qui appelle la méthode. Les deux doivent être
terminées avant le retour, y compris si l'une échoue. C'est utile pour couper
une collection en deux parties et réunir leurs résultats.

`scope` ouvre une région de travail dont toutes les tâches doivent finir avant
la sortie. Les tâches peuvent ainsi emprunter des données extérieures, comme
deux parties différentes d'un tableau, sans imposer de copie. Même les tâches
créées par ces tâches sont attendues.

La méthode supplémentaire `install` choisit le pool sur lequel exécuter un
bloc de code. C'est notamment elle qui permet aux itérateurs de trouver le pool
à utiliser. Elle accepte également des données empruntées.

Quand un worker attend un résultat Weave, il peut exécuter d'autres travaux
disponibles. Cela permet à un pool d'un seul worker d'accomplir une tâche qui
crée elle-même une sous-tâche puis attend son résultat. Les tests couvrent cette
situation, ainsi que les appels entre deux pools.

## 6. JoinHandle : le ticket de résultat

Le `JoinHandle` représente une tâche soumise. `is_done` indique si elle a
terminé, avec succès ou par une panique. `join` attend puis rend un résultat
explicite : une valeur en cas de succès, le contenu original de la panique
en cas d'échec.

Le ticket est consommé lorsqu'on récupère le résultat. Il ne permet donc pas
de récupérer deux fois la même valeur. Le jeter ne supprime pas la tâche :
celle-ci continue. Dans un scope, oublier ou jeter le ticket ne permet pas
au travail empruntant des données de sortir de la région.

Ce changement est volontairement visible dans l'API : le prototype rendait
directement une valeur ; la nouvelle version demande de traiter un `Result`.
L'application doit pouvoir décider quoi faire si son calcul échoue.

## 7. Les paniques : terminer proprement malgré un échec

Une « panique » Rust est un échec qui interrompt l'exécution normale d'une
fonction. Par exemple, une vérification obligatoire peut échouer. Weave
intercepte les paniques des tâches pour que l'échec d'une tâche ne tue pas
simplement le worker en laissant les autres attendre indéfiniment.

Pour une tâche soumise, l'échec est disponible dans le ticket. Pour une tâche
autonome, le mécanisme standard de signalement de Rust reste actif et le worker
continue. Pour `join`, les deux branches sont attendues avant de transmettre
l'échec ; si les deux échouent, celui de gauche est retenu.

Pour `scope`, tous les travaux se terminent avant de propager une panique.
Un échec de tâche soumise déjà récupéré avec son handle est considéré comme
pris en charge. Un échec non récupéré avant la fin fait échouer le scope.
Si le corps du scope échoue lui-même, cette erreur est prioritaire.

Les tests vérifient aussi qu'une destruction de résultat qui panique ne bloque
pas le scope. Ces garanties supposent le mode Rust qui déroule la pile
(`panic=unwind`). Une configuration `panic=abort` termine le processus :
aucune bibliothèque ne peut alors reprendre normalement.

## 8. Priority : choisir les travaux à traiter d'abord

Trois niveaux sont implémentés : `High`, `Normal` et `Low`. L'ordonnanceur
cherche d'abord les tâches de haute priorité, y compris dans les files des
autres workers, avant de descendre aux niveaux suivants.

Cela correspond à distinguer une commande urgente d'une opération de fond.
Une tâche déjà commencée n'est cependant pas interrompue. La priorité porte
sur le prochain travail à sélectionner ; elle n'est ni un délai garanti ni
un ordre de priorité du système d'exploitation.

Un test bloque volontairement l'unique worker, dépose plusieurs tâches de
priorités mélangées, puis le libère. L'ordre constaté confirme la sélection
des priorités. À priorité égale, les files locales et la file globale n'ont
pas la même politique : localement on prend le plus récent, globalement le
plus ancien. Le vol prélève les plus anciennes tâches locales.

Un flux permanent de tâches urgentes peut faire attendre les tâches moins
prioritaires. Cette version ne promet pas une équité automatique.

## 9. WorkerLocal : un espace de travail par employé

`WorkerLocal` crée une valeur distincte par worker, par exemple un compteur,
un tampon temporaire ou un espace de calcul réutilisable. Un worker accède à
sa propre valeur, plutôt que de partager continuellement une même valeur
avec tous les autres.

Le stockage connaît son pool propriétaire. Un appel extérieur ou venant d'un
autre pool est rejeté. Si un worker essaie d'emprunter une valeur déjà en cours
d'utilisation, l'erreur est immédiate : on évite un blocage récursif.
Si une fonction panique pendant une modification, la valeur est signalée comme
empoisonnée ; son état ne peut pas être supposé intact.

`with` est la forme simple ; `try_with` donne une erreur explicite.
`into_inner` récupère l'ensemble des valeurs lorsque le stockage est consommé.
La démonstration compte 10 000 opérations dans des compteurs par worker et
vérifie que leur somme vaut bien 10 000. La répartition exacte peut changer
d'une exécution à l'autre.

## 10. ParallelIterator et IndexedParallelIterator

Un itérateur décrit comment parcourir des éléments. Un itérateur parallèle
permet de traiter plusieurs morceaux de cette suite simultanément.
`ParallelIterator` définit les opérations de traitement et le protocole
pour distribuer leurs consommateurs.

`IndexedParallelIterator` ajoute une information essentielle : la taille
exacte et la position des éléments. Il peut couper la suite à un index précis.
Cette propriété permet notamment d'écrire chaque résultat au bon endroit
dans un tableau de destination.

Weave divise les sources en deux jusqu'à obtenir des morceaux d'au plus
512 éléments, lorsqu'il s'exécute dans un worker. Chaque petit morceau est
parcouru séquentiellement, puis les résultats sont réunis. En dehors d'un
pool, tout le parcours reste séquentiel, ce qui évite une panique liée à
l'absence de contexte.

La réunion respecte l'ordre logique gauche-droite. Cela préserve l'ordre
de `collect` et `fill`, mais n'impose pas l'ordre réel des effets de bord,
comme des messages affichés depuis plusieurs tâches.

## 11. Extensions de slices et ranges

Une slice est une vue sur une portion de tableau. Les extensions fournissent
`iter_parallel` pour lire, `iter_parallel_mut` pour modifier des éléments disjoints,
et `chunks_parallel` pour travailler par morceaux. Les tableaux et les vecteurs
bénéficient des méthodes de slices.

Les intervalles exclusifs de `usize`, par exemple `0usize..1000`, se
convertissent avec `parallelize`. Les sources vides,
les intervalles inversés et les limites de découpage sont traités explicitement.
Les autres types d'intervalles, comme les ranges inclusives, ne font pas partie
de cette implémentation.

Les morceaux de slices conservent le dernier fragment, même s'il est plus court.
Une taille de morceau nulle est rejetée. Le nom historique `ChunksAligned`
désigne un découpage respectant les frontières des morceaux, sans garantie
d'alignement mémoire pour des instructions SIMD.

## 12. for_each, map, fold, reduce et fill

`for_each` applique une action à chaque élément. Dans l'atelier, ce serait
contrôler chacune des pièces. Cette opération ne fabrique pas de collection
de résultats.

`map` transforme les éléments : doubler chaque nombre, ou fabriquer une
nouvelle valeur à partir d'une donnée. Il est paresseux : décrire la
transformation ne l'exécute pas. Le calcul démarre quand une opération finale,
comme `fill` ou `collect`, consomme l'itérateur.

`fold` construit un bilan par morceau, puis réunit les bilans. Pour calculer
une somme, chaque morceau commence à zéro, additionne ses nombres et les sommes
partielles sont additionnées. La valeur initiale doit être neutre : commencer
chaque morceau à 10 ajouterait ce 10 plusieurs fois. Le nouvel accumulateur
est déplacé à chaque étape ; il n'est plus cloné pour chaque élément.

`reduce` réunit directement les éléments, sans valeur initiale fournie.
Une suite vide renvoie `None`. La réunion doit être associative : additionner
des entiers convient, soustraire dans un regroupement arbitraire ne convient pas.
Les nombres flottants peuvent produire de petites différences d'arrondi selon
le regroupement des additions.

`fill` remplit un tableau existant. Sa taille est vérifiée avant le traitement.
Les sorties sont divisées aux mêmes indices que les entrées ; chaque tâche
écrit dans sa propre zone. Il n'y a pas de tableau intermédiaire. Si un traitement
panique, les écritures déjà faites restent présentes : ce n'est pas une transaction.

`collect`, ajouté pour compléter l'utilisation, construit un vecteur dans
l'ordre de la source. Les transformations, réductions et collectes ne demandent
pas que chaque élément soit clonable.

## 13. Validation et éléments livrés

Les tests d'intégration couvrent les tâches ordinaires, la concurrence entre
producteurs, l'exécution exactement une fois, les réveils après inactivité,
la fermeture, le vol effectif, les priorités et le stockage par worker.
Ils couvrent aussi les paniques, les descendants de scopes et les attentes
imbriquées avec un seul worker.

Les tests d'itérateurs comparent les résultats à des calculs séquentiels avec
1, 2 et 4 workers, sur des tailles allant de zéro à 65 537 éléments. Des tailles
juste avant et après le seuil de 512 sont incluses. D'autres tests vérifient
les morceaux incomplets, les mutations et les opérations paresseuses.

Trois tests documentaires doivent refuser de compiler : ils vérifient qu'une
tâche scopée ne peut pas emprunter une variable dont la durée de vie est trop
courte, qu'un handle ne peut pas emporter un tel emprunt hors du scope et qu'une
tâche descendante ne peut pas emprunter une variable temporaire de son parent.
Ce refus du compilateur est le résultat attendu.

Au total, 25 tests fonctionnels et 3 tests documentaires passent. La suite de
17 tests du pool a également été répétée 20 fois, soit 340 exécutions de tests
supplémentaires sans échec observé. Le formatage, Clippy avec refus des
avertissements et la documentation avec refus des éléments publics non
documentés passent aussi. Les tests sont exécutés en modes debug et release.

La livraison contient le code, ces tests, un README d'utilisation, une
documentation publique Rust, une démonstration `examples/tour.rs`, ce rapport
et une configuration de CI pour Windows et Linux. Les dépendances externes
inutilisées ont été retirées : cette version dépend uniquement de Rust standard.

Les vérifications locales sont réalisées sous Windows avec Rust 1.98.0.
La CI Linux est configurée, mais son résultat distant n'est pas revendiqué
avant exécution. Les tests donnent des preuves de comportement sur les cas
exercés ; ils ne démontrent pas mathématiquement tous les scénarios possibles.

## 14. Limites à présenter honnêtement

Cette version ne prétend pas surpasser Rayon. Le verrou central et le seuil
fixe constituent des choix de simplicité ; des benchmarks seraient nécessaires
pour évaluer leur coût sur de vraies charges de travail. Aucun gain chiffré
de performance n'est annoncé.

Les tâches qui ne terminent jamais empêchent une fermeture normale. Les
attentes de bibliothèques extérieures, les verrous utilisateur et les cycles
de dépendances peuvent toujours bloquer un programme. Weave facilite les
attentes de ses propres tâches, sans résoudre tous les interblocages possibles.

Deux opérations internes `unsafe` effacent temporairement les durées de vie
des fonctions empruntées. Elles sont limitées à `join` et aux scopes, avec
attente obligatoire et commentaires de sûreté. Elles n'ont pas fait l'objet
d'une vérification Miri ou d'une preuve formelle dans cette livraison.

Le pool n'est pas redimensionnable et les tâches ne sont pas annulables.
Les labels restent des métadonnées, sans interface de journalisation dédiée.
La licence de distribution doit encore être choisie par le propriétaire
avant publication.

## 15. Déroulé suggéré devant le jury

Commencer par l'atelier : quatre employés, beaucoup de pièces, un besoin de
répartition et de coordination. Expliquer ensuite le problème initial :
les mécanismes étaient esquissés, mais des attentes et des erreurs pouvaient
empêcher le travail de terminer.

Présenter les quatre façons de lancer le travail : déposer une tâche, obtenir
un ticket, réunir deux branches et ouvrir une région d'emprunt. Montrer ensuite
comment le vol de tâches et les priorités organisent les travailleurs.

Exécuter `cargo run --example tour`. La démonstration récupère 42 via un ticket,
modifie deux variables dans un scope, double 10 000 nombres et vérifie une somme
de 99 990 000. Elle affiche aussi la répartition des compteurs par worker et
le nombre de vols observés. La répartition n'est pas supposée identique à
chaque lancement.

Terminer par la validation : les tests comparent les résultats attendus, font
échouer volontairement des tâches et vérifient que le pool reste utilisable.
Préciser les limites de performance et les validations non réalisées, pour
distinguer ce qui est démontré de ce qui demanderait une campagne supplémentaire.
