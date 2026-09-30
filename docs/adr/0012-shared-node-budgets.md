# Slots et budgets partagés par node

Chaque node possède un nombre de slots d’exécution et un budget CPU, RAM et disque
local commun. Les agents alternent entre attente réseau et pics de travail ; la
réservation de leur capacité maximale bloque inutilement des exécutions.

Une exécution active ou une destination de déplacement réservée occupe un slot.
Une conversation arrêtée conserve ses données sans occuper de slot. Le manager
réserve les slots dans une transaction, conserve les leases et les droits d’accès,
et choisit automatiquement la node avec la plus grande proportion de slots libres.
Une préférence ou une fixation à une node garde son comportement existant.

Les budgets ne sont pas divisés par slot. CPU et RAM sont limités collectivement
par le cgroup v2 privé du contrôleur, qui comprend les VM et le runtime. Chaque VM
voit un plafond mémoire égal au budget de sa node moins 512 Mio pour le contrôleur,
et au maximum 32 vCPU. La marge couvre le runtime, les caches et le VMM ; elle
n'est pas divisée ou réservée par slot. Le budget minimal est de 640 Mio, avec
au moins 128 Mio visibles au guest. Ces
plafonds ne réservent pas de capacité physique. Le nombre de slots peut dépasser
le nombre de CPU. Une baisse du budget RAM sous la consommation courante et la
réserve du contrôleur est refusée ; une baisse du nombre de slots laisse finir les
exécutions déjà actives.

Le kernel courant du contrôleur est utilisé avec les systèmes de fichiers
conservés, afin de disposer du driver virtio-balloon et du signalement des pages
libres. Firecracker rend ces pages à l’hôte. Au-delà de 85 % du budget RAM, le
contrôleur demande progressivement aux guests de récupérer de la mémoire libre
et des caches. Le balloon se dégonfle en cas de manque de mémoire dans le guest.
Cette coopération est un mécanisme de récupération, pas une garantie de mémoire
disponible : le cgroup conserve le plafond physique. Les admissions s’arrêtent
lorsque la mémoire ou le disque manquent de marge. Des pics simultanés peuvent
encore épuiser le budget ; le nombre de slots doit être adapté au travail réel.
La mesure de pression porte sur tout le conteneur : déplacer le contrôleur dans
son cgroup délégué ne déplace pas les charges mémoire déjà créées. Elle exclut
le cache de fichiers inactif que Linux peut récupérer,
afin que l’import de l’image ne bloque pas les admissions. Lors d’une baisse du
budget RAM, le contrôleur demande d’abord la récupération des pages de fichiers,
puis vérifie la consommation totale avant de changer le plafond.

Le budget disque porte sur les octets locaux des conversations, y compris les
journaux, les caches propres et les anciennes copies. Les volumes S3 restent
privés et chargés à la demande. Les écritures utilisent l’admission commune et
débitent une estimation conservatrice, réconciliée avec l’allocation physique une
fois par seconde. La réserve du filesystem hôte est préservée en dehors du quota commun,
même lorsque ce quota est inférieur à la réserve. Aucun journal non publié n’est évincé. Un nouveau volume ext4 possède
une capacité logique égale au budget disque de sa node, sans réserver cette
capacité locale ; un disque conservé garde sa taille lors des déplacements.

`list_nodes` expose les budgets communs, les slots disponibles et la pression
courante. `move_to_node` reçoit une destination et éventuellement un délai
d’attente. Les agents ne demandent plus de CPU, RAM ou disque. Les contrôles par
conversation et les limites de ressources par agent disparaissent du web et
d’Android. Les anciennes demandes enregistrées sont ignorées par le placement ;
les anciens paramètres envoyés à une demande de déplacement sont refusés.

Les budgets et les slots sont persistés sur le manager et appliqués par le
contrôleur via une interface authentifiée. Les heartbeats vérifient leur
application avant d’admettre de nouvelles exécutions. Un ancien runtime qui ne
supporte pas les budgets partagés cesse d’être éligible ; les leases existantes
et le parcours de mise à jour restent disponibles. Le protocole d’installation
reste compatible avec le superviseur déjà installé.

Références : [Firecracker ballooning v1.17.0](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/ballooning.md),
[cgroup v2](https://docs.kernel.org/admin-guide/cgroup-v2.html).

Les préparations initiales des VM sont sérialisées jusqu’à la réservation durable
du slot ; les VM admises utilisent ensuite tous les slots des nodes, sans plafond
global `CONCURRENCY`. Ce dernier reste applicable aux exécutions directes sur
l’hôte en développement. Le plafond généré de 256 processus du conteneur est
supprimé : il comptait les threads vCPU de Firecracker, pas les processus invités.
Un plafond explicitement personnalisé est conservé par le migrateur Compose.

Les capacités CPU/RAM sont détectées à chaque démarrage depuis l’enveloppe du
conteneur, indépendamment du budget du cgroup partagé. Un budget conservé qui
dépasse une nouvelle enveloppe plus petite est réduit au plafond réel ; les
budgets demandés par le manager restent visibles avec le motif de refus tant
qu’ils ne peuvent pas être appliqués. Les heartbeats continuent pendant ce refus.
