"""
@brief Decoder for event data

This decoder takes in serialized events, parses them, and packages the results
in event_data objects.

Example data structure:
    +-------------------+---------------------+---------------------- - - -
    | ID (4 bytes)      | Time Tag (11 bytes) | Event argument data....
    +-------------------+---------------------+---------------------- - - -

@date Created June 29, 2018
@author R. Joseph Paetz

@bug No known bugs
"""

from fprime_gds.common.models.serialize import time_type
from fprime_gds.common.models.serialize.type_exceptions import TypeException

from fprime_gds.common.data_types import event_data
from fprime_gds.common.decoders import decoder
from fprime_gds.common.decoders.decoder import DecodingException
from fprime_gds.common.utils.config_manager import ConfigManager
from fprime_gds.common.utils.opcode_mask import unmask_opcode

import logging

LOGGER = logging.getLogger("event_decoder")


class EventDecoder(decoder.Decoder):
    """Decoder class for event data"""

    def __init__(self, event_dict):
        """
        EventDecoder class constructor

        Args:
            event_dict: Event dictionary. Event IDs should be keys and
                        EventTemplate objects should be values

        Returns:
            An initialized EventDecoder object.
        """
        super(EventDecoder, self).__init__()

        self.__dict = event_dict
        FwEventIdType = ConfigManager().get_type("FwEventIdType")
        self.id_obj = FwEventIdType()

    def decode_api(self, data):
        """
        Decodes the given data and returns the result.

        This function allows for non-registered code to call the same decoding
        code as is used to parse data passed to the data_callback function.

        Args:
            data: Binary data to decode

        Returns:
            Parsed version of the event data in the form of a EventData object
            or None if the data is not decodable
        """
        ptr = 0

        event_list = []

        while ptr < len(data):

            # Decode event ID here...
            if ptr + self.id_obj.getSize() <= len(data):
                self.id_obj.deserialize(data, ptr)
            else:
                LOGGER.warning("Insufficient data for event ID")
                break
            ptr += self.id_obj.getSize()
            event_id = self.id_obj.val

            # Decode time...
            event_time = time_type.TimeType()
            event_time.deserialize(data, ptr)
            ptr += event_time.getSize()

            if event_id not in self.__dict:
                msg = f"Event {event_id} not found in dictionary"
                raise DecodingException(msg)

            event_temp = self.__dict[event_id]

            (size, arg_vals) = self.decode_args(data, ptr, event_temp)

            self.unmask_opcode_args(event_temp, arg_vals)

            event_list.append(event_data.EventData(arg_vals, event_time, event_temp))
            # add up argument sizes
            ptr += size
        return event_list

    @staticmethod
    def unmask_opcode_args(template, arg_vals):
        """
        Unmasks opcode-bearing event arguments in-place when opcode masking
        is enabled via ConfigManager.

        When the flight side masks command opcodes before emitting them in
        events (see Svc::CmdDispatcherCfg::getEventOpcode()), this inverts the
        mask so displayed events show real opcodes. Arguments are matched by
        name against the configured "opcode_mask_arg_names" set. No-op unless
        "opcode_mask_enabled" is True and "opcode_mask_keys" is set.

        Args:
            template: EventTemplate for the decoded event
            arg_vals: Tuple of decoded argument value objects (mutated in-place)
        """
        config = ConfigManager()
        if not config.get_config("opcode_mask_enabled"):
            return
        keys = config.get_config("opcode_mask_keys")
        if not keys:
            return
        arg_names = config.get_config("opcode_mask_arg_names")
        for arg, arg_obj in zip(template.get_args(), arg_vals):
            (arg_name, _, _) = arg
            if arg_name in arg_names and isinstance(arg_obj.val, int):
                width = arg_obj.getSize() * 8
                arg_obj.val = unmask_opcode(arg_obj.val, keys, width)

    @staticmethod
    def decode_args(arg_data, offset, template):
        """
        Decodes the serialized event arguments

        The event arguments are each serialized and then appended to each other.
        Parse that section of the data into the individual arguments.

        Args:
            arg_data: Serialized argument data to parse
            offset: Offset into the arg_data where parsing should start
            template: EventTemplate object that describes the type of event the
                      arg_data goes to.

        Returns:
            Parsed arguments in a tuple (order the same as they were parsed in).
            Each element in the tuple is an instance of the same class as the
            corresponding arg_type object in the template parameter. Returns
            none if the arguments can't be parsed
        """
        arg_results = []
        args = template.get_args()

        # For each argument, use the arg_obj deserialize method to get the value
        for arg in args:
            (arg_name, arg_desc, arg_type) = arg

            arg_obj = arg_type()

            try:
                arg_obj.deserialize(arg_data, offset)
                arg_results.append(arg_obj)
            except TypeException as e:
                msg = f"Event argument decoding failed {e.getMsg()}"
                raise DecodingException(msg)

            offset = offset + arg_obj.getSize()

        return [offset, tuple(arg_results)]
