---- MODULE GroupSenderKeys ----
EXTENDS FiniteSets, Naturals

CONSTANTS
    Devices,
    InitialMembers,
    InitialAdmins,
    MaxEpoch,
    AllowUnauthorizedMembershipMutations

ASSUME Devices # {}
ASSUME InitialMembers \subseteq Devices
ASSUME InitialAdmins \subseteq InitialMembers
ASSUME InitialAdmins # {}
ASSUME MaxEpoch \in Nat \ {0}

Epochs == 1..MaxEpoch

MsgPair(d, e) == <<d, e>>
DistItem(d, e) == <<"dist", d, e>>
MsgItem(d, e) == <<"msg", d, e>>

Items ==
    {DistItem(d, e) : d \in Devices, e \in Epochs}
        \cup
    {MsgItem(d, e) : d \in Devices, e \in Epochs}

ItemKind(i) == i[1]
ItemTarget(i) == i[2]
ItemEpoch(i) == i[3]

HasRelayFor(d, bag) ==
    \E i \in bag: ItemTarget(i) = d

HasPairFor(d, pairs) ==
    \E e \in Epochs: <<d, e>> \in pairs

VARIABLES
    epoch,
    members,
    admins,
    joinedAt,
    revoked,
    revokedFrom,
    cleanupDone,
    distKnown,
    needDist,
    needMsg,
    relayBag,
    blocked,
    delivered,
    relayUp,
    badAuthMutation,
    badDecryptWithoutDistribution,
    badPreJoinDecrypt,
    badPostRevocationDecrypt

vars ==
    <<epoch, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
      distKnown, needDist, needMsg, relayBag, blocked, delivered, relayUp,
      badAuthMutation, badDecryptWithoutDistribution, badPreJoinDecrypt,
      badPostRevocationDecrypt>>

Init ==
    /\ epoch = 1
    /\ members = InitialMembers
    /\ admins = InitialAdmins
    /\ joinedAt = [d \in Devices |-> IF d \in InitialMembers THEN 1 ELSE 0]
    /\ revoked = {}
    /\ revokedFrom = [d \in Devices |-> 0]
    /\ cleanupDone = {}
    /\ distKnown = [d \in Devices |-> {}]
    /\ needDist = {}
    /\ needMsg = {}
    /\ relayBag = {}
    /\ blocked = {}
    /\ delivered = {}
    /\ relayUp = TRUE
    /\ badAuthMutation = FALSE
    /\ badDecryptWithoutDistribution = FALSE
    /\ badPreJoinDecrypt = FALSE
    /\ badPostRevocationDecrypt = FALSE

AddMember(actor, d) ==
    /\ actor \in Devices
    /\ d \in Devices
    /\ d \notin members
    /\ d \notin revoked
    /\ joinedAt[d] = 0
    /\ (actor \in admins \/ AllowUnauthorizedMembershipMutations)
    /\ members' = members \cup {d}
    /\ joinedAt' = [joinedAt EXCEPT ![d] = epoch]
    /\ badAuthMutation' = (badAuthMutation \/ ~(actor \in admins))
    /\ UNCHANGED <<
        epoch, admins, revoked, revokedFrom, cleanupDone, distKnown,
        needDist, needMsg, relayBag, blocked, delivered, relayUp,
        badDecryptWithoutDistribution, badPreJoinDecrypt, badPostRevocationDecrypt
      >>

RemoveMember(actor, d) ==
    /\ actor \in Devices
    /\ d \in members
    /\ d # actor
    /\ (actor \in admins \/ AllowUnauthorizedMembershipMutations)
    /\ LET nextEpoch == IF epoch < MaxEpoch THEN epoch + 1 ELSE epoch IN
        /\ epoch' = nextEpoch
        /\ members' = members \ {d}
        /\ admins' = admins \ {d}
        /\ revoked' = revoked \cup {d}
        /\ revokedFrom' =
            [revokedFrom EXCEPT ![d] = IF @ = 0 THEN nextEpoch ELSE @]
        /\ cleanupDone' = cleanupDone \ {d}
    /\ badAuthMutation' = (badAuthMutation \/ ~(actor \in admins))
    /\ UNCHANGED <<
        joinedAt, distKnown, needDist, needMsg, relayBag, blocked,
        delivered, relayUp, badDecryptWithoutDistribution,
        badPreJoinDecrypt, badPostRevocationDecrypt
      >>

AddAdmin(actor, d) ==
    /\ actor \in Devices
    /\ d \in members
    /\ d \notin admins
    /\ (actor \in admins \/ AllowUnauthorizedMembershipMutations)
    /\ admins' = admins \cup {d}
    /\ badAuthMutation' = (badAuthMutation \/ ~(actor \in admins))
    /\ UNCHANGED <<
        epoch, members, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needDist, needMsg, relayBag, blocked, delivered, relayUp,
        badDecryptWithoutDistribution, badPreJoinDecrypt, badPostRevocationDecrypt
      >>

RemoveAdmin(actor, d) ==
    /\ actor \in Devices
    /\ d \in admins
    /\ Cardinality(admins) > 1
    /\ (actor \in admins \/ AllowUnauthorizedMembershipMutations)
    /\ admins' = admins \ {d}
    /\ badAuthMutation' = (badAuthMutation \/ ~(actor \in admins))
    /\ UNCHANGED <<
        epoch, members, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needDist, needMsg, relayBag, blocked, delivered, relayUp,
        badDecryptWithoutDistribution, badPreJoinDecrypt, badPostRevocationDecrypt
      >>

RotateEpoch(actor) ==
    /\ actor \in admins
    /\ epoch < MaxEpoch
    /\ epoch' = epoch + 1
    /\ UNCHANGED <<
        members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needDist, needMsg, relayBag, blocked, delivered,
        relayUp, badAuthMutation, badDecryptWithoutDistribution,
        badPreJoinDecrypt, badPostRevocationDecrypt
      >>

QueueDistribution(sender) ==
    /\ sender \in members
    /\ members \ {sender} # {}
    /\ LET pairs == {<<d, epoch>> : d \in members \ {sender}}
           newPairs == pairs \ needDist IN
        /\ newPairs # {}
        /\ needDist' = needDist \cup newPairs
        /\ relayBag' = relayBag \cup {DistItem(p[1], p[2]) : p \in newPairs}
    /\ UNCHANGED <<
        epoch, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needMsg, blocked, delivered, relayUp,
        badAuthMutation, badDecryptWithoutDistribution,
        badPreJoinDecrypt, badPostRevocationDecrypt
      >>

QueueMessage(sender) ==
    /\ sender \in members
    /\ members \ {sender} # {}
    /\ LET pairs == {<<d, epoch>> : d \in members \ {sender}}
           newPairs == pairs \ needMsg IN
        /\ newPairs # {}
        /\ needMsg' = needMsg \cup newPairs
        /\ relayBag' = relayBag \cup {MsgItem(p[1], p[2]) : p \in newPairs}
    /\ UNCHANGED <<
        epoch, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needDist, blocked, delivered, relayUp,
        badAuthMutation, badDecryptWithoutDistribution,
        badPreJoinDecrypt, badPostRevocationDecrypt
      >>

RetryDist(d, e) ==
    /\ d \in Devices
    /\ e \in Epochs
    /\ <<d, e>> \in needDist
    /\ DistItem(d, e) \notin relayBag
    /\ relayBag' = relayBag \cup {DistItem(d, e)}
    /\ UNCHANGED <<
        epoch, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needDist, needMsg, blocked, delivered, relayUp,
        badAuthMutation, badDecryptWithoutDistribution,
        badPreJoinDecrypt, badPostRevocationDecrypt
      >>

RetryMsg(d, e) ==
    /\ d \in Devices
    /\ e \in Epochs
    /\ <<d, e>> \in needMsg
    /\ MsgItem(d, e) \notin relayBag
    /\ relayBag' = relayBag \cup {MsgItem(d, e)}
    /\ UNCHANGED <<
        epoch, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needDist, needMsg, blocked, delivered, relayUp,
        badAuthMutation, badDecryptWithoutDistribution,
        badPreJoinDecrypt, badPostRevocationDecrypt
      >>

CleanupRemoved(d) ==
    /\ d \in revoked
    /\ d \notin cleanupDone
    /\ cleanupDone' = cleanupDone \cup {d}
    /\ distKnown' = [distKnown EXCEPT ![d] = {}]
    /\ needDist' = {p \in needDist : p[1] # d}
    /\ needMsg' = {p \in needMsg : p[1] # d}
    /\ blocked' = {p \in blocked : p[1] # d}
    /\ relayBag' = {i \in relayBag : ItemTarget(i) # d}
    /\ UNCHANGED <<
        epoch, members, admins, joinedAt, revoked, revokedFrom, delivered,
        relayUp, badAuthMutation, badDecryptWithoutDistribution,
        badPreJoinDecrypt, badPostRevocationDecrypt
      >>

RelayDeliverDistToMember(d, e) ==
    /\ d \in members
    /\ e \in Epochs
    /\ relayUp
    /\ DistItem(d, e) \in relayBag
    /\ relayBag' = relayBag \ {DistItem(d, e)}
    /\ distKnown' = [distKnown EXCEPT ![d] = @ \cup {e}]
    /\ needDist' = needDist \ {<<d, e>>}
    /\ blocked' = blocked \ {<<d, e>>}
    /\ delivered' =
        IF <<d, e>> \in blocked
            THEN delivered \cup {<<d, e>>}
            ELSE delivered
    /\ needMsg' =
        IF <<d, e>> \in blocked
            THEN needMsg \ {<<d, e>>}
            ELSE needMsg
    /\ badPreJoinDecrypt' =
        (badPreJoinDecrypt
            \/ (<<d, e>> \in blocked /\ (joinedAt[d] = 0 \/ e < joinedAt[d])))
    /\ badPostRevocationDecrypt' =
        (badPostRevocationDecrypt
            \/ (<<d, e>> \in blocked /\ revokedFrom[d] # 0 /\ e >= revokedFrom[d]))
    /\ UNCHANGED <<
        epoch, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        relayUp, badAuthMutation, badDecryptWithoutDistribution
      >>

RelayDeliverDistDropped(d, e) ==
    /\ d \notin members
    /\ e \in Epochs
    /\ relayUp
    /\ DistItem(d, e) \in relayBag
    /\ relayBag' = relayBag \ {DistItem(d, e)}
    /\ needDist' = needDist \ {<<d, e>>}
    /\ UNCHANGED <<
        epoch, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needMsg, blocked, delivered, relayUp,
        badAuthMutation, badDecryptWithoutDistribution,
        badPreJoinDecrypt, badPostRevocationDecrypt
      >>

RelayDeliverMsgDecrypt(d, e) ==
    /\ d \in members
    /\ e \in Epochs
    /\ e \in distKnown[d]
    /\ relayUp
    /\ MsgItem(d, e) \in relayBag
    /\ relayBag' = relayBag \ {MsgItem(d, e)}
    /\ delivered' = delivered \cup {<<d, e>>}
    /\ needMsg' = needMsg \ {<<d, e>>}
    /\ badDecryptWithoutDistribution' =
        (badDecryptWithoutDistribution \/ ~(e \in distKnown[d]))
    /\ badPreJoinDecrypt' =
        (badPreJoinDecrypt \/ (joinedAt[d] = 0 \/ e < joinedAt[d]))
    /\ badPostRevocationDecrypt' =
        (badPostRevocationDecrypt \/ (revokedFrom[d] # 0 /\ e >= revokedFrom[d]))
    /\ UNCHANGED <<
        epoch, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needDist, blocked, relayUp, badAuthMutation
      >>

RelayDeliverMsgBlocked(d, e) ==
    /\ d \in members
    /\ e \in Epochs
    /\ e \notin distKnown[d]
    /\ relayUp
    /\ MsgItem(d, e) \in relayBag
    /\ relayBag' = relayBag \ {MsgItem(d, e)}
    /\ blocked' = blocked \cup {<<d, e>>}
    /\ UNCHANGED <<
        epoch, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needDist, needMsg, delivered, relayUp, badAuthMutation,
        badDecryptWithoutDistribution, badPreJoinDecrypt,
        badPostRevocationDecrypt
      >>

RelayDeliverMsgDropped(d, e) ==
    /\ d \notin members
    /\ e \in Epochs
    /\ relayUp
    /\ MsgItem(d, e) \in relayBag
    /\ relayBag' = relayBag \ {MsgItem(d, e)}
    /\ needMsg' = needMsg \ {<<d, e>>}
    /\ UNCHANGED <<
        epoch, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needDist, blocked, delivered, relayUp, badAuthMutation,
        badDecryptWithoutDistribution, badPreJoinDecrypt,
        badPostRevocationDecrypt
      >>

RelayDrop(i) ==
    /\ i \in relayBag
    /\ relayBag' = relayBag \ {i}
    /\ UNCHANGED <<
        epoch, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needDist, needMsg, blocked, delivered, relayUp,
        badAuthMutation, badDecryptWithoutDistribution,
        badPreJoinDecrypt, badPostRevocationDecrypt
      >>

RelayDuplicate(i) ==
    /\ i \in relayBag
    /\ UNCHANGED vars

RelayPartition ==
    /\ relayUp
    /\ relayUp' = FALSE
    /\ UNCHANGED <<
        epoch, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needDist, needMsg, relayBag, blocked, delivered,
        badAuthMutation, badDecryptWithoutDistribution,
        badPreJoinDecrypt, badPostRevocationDecrypt
      >>

RelayRecover ==
    /\ ~relayUp
    /\ relayUp' = TRUE
    /\ UNCHANGED <<
        epoch, members, admins, joinedAt, revoked, revokedFrom, cleanupDone,
        distKnown, needDist, needMsg, relayBag, blocked, delivered,
        badAuthMutation, badDecryptWithoutDistribution,
        badPreJoinDecrypt, badPostRevocationDecrypt
      >>

RelayDelay ==
    /\ relayBag # {}
    /\ UNCHANGED vars

Stutter == UNCHANGED vars

Next ==
    \/ \E actor \in Devices, d \in Devices: AddMember(actor, d)
    \/ \E actor \in Devices, d \in Devices: RemoveMember(actor, d)
    \/ \E actor \in Devices, d \in Devices: AddAdmin(actor, d)
    \/ \E actor \in Devices, d \in Devices: RemoveAdmin(actor, d)
    \/ \E actor \in Devices: RotateEpoch(actor)
    \/ \E sender \in Devices: QueueDistribution(sender)
    \/ \E sender \in Devices: QueueMessage(sender)
    \/ \E d \in Devices, e \in Epochs: RetryDist(d, e)
    \/ \E d \in Devices, e \in Epochs: RetryMsg(d, e)
    \/ \E d \in Devices: CleanupRemoved(d)
    \/ \E d \in Devices, e \in Epochs: RelayDeliverDistToMember(d, e)
    \/ \E d \in Devices, e \in Epochs: RelayDeliverDistDropped(d, e)
    \/ \E d \in Devices, e \in Epochs: RelayDeliverMsgDecrypt(d, e)
    \/ \E d \in Devices, e \in Epochs: RelayDeliverMsgBlocked(d, e)
    \/ \E d \in Devices, e \in Epochs: RelayDeliverMsgDropped(d, e)
    \/ \E i \in Items: RelayDrop(i)
    \/ \E i \in Items: RelayDuplicate(i)
    \/ RelayPartition
    \/ RelayRecover
    \/ RelayDelay
    \/ Stutter

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ \A s \in Devices: WF_vars(QueueDistribution(s))
    /\ \A s \in Devices: WF_vars(QueueMessage(s))
    /\ \A d \in Devices, e \in Epochs: WF_vars(RetryDist(d, e))
    /\ \A d \in Devices, e \in Epochs: WF_vars(RetryMsg(d, e))
    /\ \A d \in Devices: WF_vars(CleanupRemoved(d))
    /\ \A d \in Devices, e \in Epochs: SF_vars(RelayDeliverDistToMember(d, e))
    /\ \A d \in Devices, e \in Epochs: SF_vars(RelayDeliverMsgDecrypt(d, e))

SpecUnderRecovery ==
    /\ Spec
    /\ <>[]relayUp

GroupStateValid ==
    /\ admins \subseteq members
    /\ admins # {}

NoUnauthorizedMembershipMutation ==
    ~badAuthMutation

NoDecryptWithoutDistribution ==
    ~badDecryptWithoutDistribution

NoPreJoinDecrypt ==
    ~badPreJoinDecrypt

NoPostRevocationDecrypt ==
    ~badPostRevocationDecrypt

BlockedPendingDistribution ==
    \A d \in Devices, e \in Epochs:
        <<d, e>> \in blocked => e \notin distKnown[d]

NoTransportForRemovedAfterCleanup ==
    \A d \in cleanupDone:
        /\ d \notin members
        /\ d \notin admins
        /\ distKnown[d] = {}
        /\ ~HasRelayFor(d, relayBag)
        /\ ~HasPairFor(d, needDist)
        /\ ~HasPairFor(d, needMsg)
        /\ ~HasPairFor(d, blocked)

NeededDistEventuallyKnownUnderRecovery ==
    \A d \in Devices, e \in Epochs:
        [](
            (<<d, e>> \in needDist
              /\ d \in members
              /\ revokedFrom[d] = 0
              /\ <>[](<<d, e>> \in needDist /\ d \in members /\ revokedFrom[d] = 0))
                => <>(e \in distKnown[d])
          )

NeededMsgEventuallyDeliveredUnderRecovery ==
    \A d \in Devices, e \in Epochs:
        [](
            (<<d, e>> \in needMsg
              /\ d \in members
              /\ revokedFrom[d] = 0
              /\ e \in distKnown[d]
              /\ <>[](<<d, e>> \in needMsg /\ d \in members /\ revokedFrom[d] = 0 /\ e \in distKnown[d]))
                => <>(<<d, e>> \in delivered)
          )

RevokedEventuallyPurged ==
    \A d \in Devices:
        [](
            (d \in revoked)
                => <>(d \in cleanupDone
                      /\ d \notin members
                      /\ d \notin admins
                      /\ distKnown[d] = {}
                      /\ ~HasRelayFor(d, relayBag)
                      /\ ~HasPairFor(d, needDist)
                      /\ ~HasPairFor(d, needMsg)
                      /\ ~HasPairFor(d, blocked))
          )

====
